//! Executes a tree of browser tests.
//!
//! Suites run recursively: a suite starts its entries in order, at most its parallelism at once.
//! Every test runs in a browser session taken from the [`SessionPool`], which keeps spare
//! sessions ready so that a starting test rarely waits for a browser to start. A session's
//! lifetime is bound to a scoped session API ([`SessionFactory::run_session`], that of
//! `chrome-for-testing-manager` in a real run), so each session is owned by a worker future that
//! creates it, offers it to the pool, runs the test it is assigned, and quits it. With [`SessionReuse`] enabled, a worker resets its session after the test and
//! offers it to the pool again instead, as long as upcoming tests need sessions. Worker futures are
//! driven by [`drive_workers`]. Everything crossing between them and the executor is plain data
//! (test indices, channels), never a borrow of the run's state.
//!
//! - [`plan`] numbers the tests and completes their settings with the runner's.
//! - [`session_pool`] decides when sessions are created, reset, or quit.
//! - [`sessions`] creates sessions and acts on them, with Chrome for Testing or, in tests, fakes.
//! - [`worker`] owns each session, and [`test_run`] runs a test in it, within the test's time
//!   limit, and turns what happened into its record and report.

use std::{
    future::Future,
    pin::{Pin, pin},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use chrome_for_testing_manager::{CancellationToken, ChromeForTesting};
use futures_util::StreamExt as _;
use rootcause::Report;
use thirtyfour::{ChromeCapabilities, error::WebDriverResult};
use tokio::sync::{mpsc, oneshot};

mod failures;
#[cfg(test)]
mod fake_sessions;
mod plan;
mod session_pool;
mod sessions;
mod test_run;
mod worker;

use self::{
    failures::BrowserTestFailures,
    plan::{Node, QueuedTest, Suite, plan},
    session_pool::{CreationFailure, Delivery, SessionPool, Upcoming},
    sessions::{ChromeSessions, SessionFactory},
    worker::{Job, drive_workers},
};
use crate::{
    BrowserTestError, FailurePolicy, ProgressWarnings, SessionReuse, TestSuite,
    driver_output::DriverOutputCapture,
    profile::RunProfiles,
    progress::{Activity, ProgressBoard},
    report::{
        FormatDuration, SessionPreparation, SessionTiming, SuiteRecord, TestOutcome, TestRecord,
        timing_breakdown,
    },
    test_settings::TestDefaults,
};

pub(crate) type ChromeCapabilitiesSetup =
    dyn Fn(&mut ChromeCapabilities) -> WebDriverResult<()> + Send + Sync + 'static;

/// Everything the runner configured for executing tests.
pub(crate) struct ExecutionConfig<'a> {
    pub(crate) chrome: &'a ChromeForTesting,
    pub(crate) visible: bool,
    /// The runner's settings, for tests that don't set their own.
    pub(crate) test_defaults: TestDefaults,
    pub(crate) chrome_capabilities_setups: &'a [Arc<ChromeCapabilitiesSetup>],
    /// Where sessions get their profiles.
    pub(crate) profiles: &'a RunProfiles,
    /// Canceled when the run is cancelled: no further tests start, and running ones are cancelled.
    pub(crate) cancellation: &'a CancellationToken,
    pub(crate) failure_policy: FailurePolicy,
    pub(crate) progress_warnings: ProgressWarnings,
    /// Spare sessions kept ready. `None`: [`default_spare_sessions`].
    pub(crate) spare_sessions: Option<usize>,
    pub(crate) session_reuse: SessionReuse,
    /// The capture of the driver's output, whose lines of a failed test's session its report
    /// shows.
    pub(crate) driver_output: Option<&'a DriverOutputCapture>,
}

/// What executing a test tree produced.
pub(crate) struct Execution {
    /// A record per started test, in test order.
    pub(crate) records: Vec<TestRecord>,
    /// The timing of every named suite, in definition order.
    pub(crate) suites: Vec<SuiteRecord>,
    /// Time spent quitting sessions without a test.
    pub(crate) released_session_teardown: Duration,
    /// How many tests did not start, after a fail-fast stop or a cancellation.
    pub(crate) not_started: usize,
    pub(crate) result: Result<(), Report<BrowserTestError>>,
}

/// Run every test of `tests`.
pub(crate) async fn execute_tests<Context>(
    config: &ExecutionConfig<'_>,
    context: &Context,
    tests: TestSuite<Context>,
) -> Execution
where
    Context: Sync + ?Sized,
{
    let sessions = ChromeSessions {
        chrome: config.chrome,
        visible: config.visible,
        chrome_capabilities_setups: config.chrome_capabilities_setups,
        profiles: config.profiles,
        session_reuse: config.session_reuse,
        cancellation: config.cancellation,
    };
    let settings = RunSettings {
        test_defaults: config.test_defaults,
        cancellation: config.cancellation,
        failure_policy: config.failure_policy,
        progress_warnings: config.progress_warnings,
        spare_sessions: config.spare_sessions,
        session_reuse: config.session_reuse,
        driver_output: config.driver_output,
    };
    execute(&sessions, settings, context, tests).await
}

/// The settings of [`ExecutionConfig`] that don't concern how sessions are created.
#[derive(Clone, Copy)]
struct RunSettings<'a> {
    test_defaults: TestDefaults,
    cancellation: &'a CancellationToken,
    failure_policy: FailurePolicy,
    progress_warnings: ProgressWarnings,
    spare_sessions: Option<usize>,
    session_reuse: SessionReuse,
    /// The capture of the driver's output, whose lines of a failed test's session its report
    /// shows.
    driver_output: Option<&'a DriverOutputCapture>,
}

/// Run every test of `tests` in sessions of `sessions`.
async fn execute<Context, S>(
    sessions: &S,
    config: RunSettings<'_>,
    context: &Context,
    tests: TestSuite<Context>,
) -> Execution
where
    Context: Sync + ?Sized,
    S: SessionFactory,
{
    let (root, prepared) = plan(tests, config.test_defaults);
    let max_concurrency = root.max_concurrency();
    let pooled = |fresh: bool, only_run_always: bool| {
        prepared
            .iter()
            .filter(|test| {
                matches!(&test.prepared, Ok(prepared)
                    if prepared.pooled
                        && prepared.fresh_session == fresh
                        && (test.placement.run_always || !only_run_always))
            })
            .count()
    };
    let upcoming = Upcoming {
        any: pooled(false, false),
        fresh: pooled(true, false),
    };
    let upcoming_always = Upcoming {
        any: pooled(false, true),
        fresh: pooled(true, true),
    };
    let (requests_tx, requests_rx) = mpsc::unbounded_channel();
    let env = Env {
        config,
        context,
        tests: prepared,
        results: Mutex::default(),
        board: ProgressBoard::default(),
        pool: SessionPool::new(
            max_concurrency,
            config
                .spare_sessions
                .unwrap_or_else(|| default_spare_sessions(max_concurrency, config.session_reuse)),
            upcoming,
            upcoming_always,
            config.test_defaults.element_query_wait,
            requests_tx,
            config.cancellation.clone(),
        ),
    };

    env.pool.replenish();
    let execution = async {
        run_suite(&env, &root).await;
        env.pool.close();
    };
    let run = futures_util::future::join(execution, drive_workers(&env, sessions, requests_rx));
    // The watchdog never finishes on its own. It is dropped once the run is done.
    let watchdog = env.board.watch(config.progress_warnings);
    futures_util::future::select(pin!(run), pin!(watchdog)).await;
    env.pool.debug_assert_settled();

    let Env { tests, results, .. } = env;
    let Results {
        failures,
        mut records,
        mut suites,
        released_session_teardown,
    } = results.into_inner().unwrap_or_else(PoisonError::into_inner);
    let not_started = tests.len() - records.len();
    records.sort_by_key(|record| record.index);
    suites.sort_by_key(|(id, _)| *id);
    Execution {
        records,
        suites: suites.into_iter().map(|(_, suite)| suite).collect(),
        released_session_teardown,
        not_started,
        result: failures.into_result(),
    }
}

/// Spare sessions when the runner sets none: one per test that can run at the same time, as a
/// new session starts a browser. Reused sessions only need resetting (about 0.1s), so then one per
/// eight tests suffices to keep tests from waiting: measured with 821 tests at parallelism 8, one
/// spare ran as fast as eight, with seven browsers less.
fn default_spare_sessions(max_concurrency: usize, session_reuse: SessionReuse) -> usize {
    if session_reuse.is_enabled() {
        max_concurrency.div_ceil(8)
    } else {
        max_concurrency
    }
}

/// State of one run, shared by the executor and the session workers.
struct Env<'c, Context>
where
    Context: Sync + ?Sized,
{
    config: RunSettings<'c>,
    context: &'c Context,
    tests: Vec<QueuedTest<Context>>,
    results: Mutex<Results>,
    board: ProgressBoard,
    pool: SessionPool,
}

/// What a run's tests produced so far.
#[derive(Default)]
struct Results {
    failures: BrowserTestFailures,
    /// A record per finished test, in the order they finished.
    records: Vec<TestRecord>,
    /// The timing of every finished named suite, with its id.
    suites: Vec<(usize, SuiteRecord)>,
    /// Time spent quitting sessions without a test.
    released_session_teardown: Duration,
}

impl<Context> Env<'_, Context>
where
    Context: Sync + ?Sized,
{
    /// The test at `index`.
    fn test(&self, index: usize) -> &QueuedTest<Context> {
        &self.tests[index]
    }

    /// With [`FailurePolicy::FailFast`], start no further tests (after a failure).
    fn stop_starting_on_fail_fast(&self) {
        if self.config.failure_policy == FailurePolicy::FailFast {
            self.pool.stop();
        }
    }

    /// Record a finished test.
    fn finish(&self, mut record: TestRecord, result: Result<(), Report<BrowserTestError>>) {
        record.failure = result.as_ref().err().map(ToString::to_string);
        let total = FormatDuration(record.total());
        let breakdown = timing_breakdown(&record);
        match record.outcome {
            TestOutcome::Passed => tracing::debug!(
                test = %record.name,
                total_ms = record.total().as_millis(),
                "Browser test '{}' passed in {total} ({breakdown}).",
                record.name,
            ),
            TestOutcome::Failed | TestOutcome::Panicked => tracing::error!(
                test = %record.name,
                total_ms = record.total().as_millis(),
                "Browser test '{}' failed in {total} ({breakdown}).",
                record.name,
            ),
            TestOutcome::TimedOut => tracing::error!(
                test = %record.name,
                total_ms = record.total().as_millis(),
                "Browser test '{}' timed out after {total} ({breakdown}).",
                record.name,
            ),
            TestOutcome::Cancelled => tracing::warn!(
                test = %record.name,
                total_ms = record.total().as_millis(),
                "Browser test '{}' was cancelled after {total} ({breakdown}).",
                record.name,
            ),
        }
        if result.is_err() {
            self.stop_starting_on_fail_fast();
        }
        let mut results = self.results();
        if let Err(report) = result {
            results.failures.push(record.index, report);
        }
        results.records.push(record);
    }

    fn results(&self) -> MutexGuard<'_, Results> {
        self.results.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tell the test waiting for the session of `delivery` that it could not be created.
    fn fail_creation(&self, delivery: Delivery, report: Report, creation_start: Instant) {
        let failure = CreationFailure {
            report,
            preparation: SessionPreparation::Created(creation_start.elapsed()),
        };
        self.pool.deliver(delivery, Err(failure));
    }

    fn warn_if_slow_session(&self, what: &str, duration: Duration) {
        if let Some(threshold) = self.config.progress_warnings.session()
            && duration > threshold
        {
            tracing::warn!(
                duration_ms = duration.as_millis(),
                "{what} took {}.",
                FormatDuration(duration),
            );
        }
    }
}

/// The session a test was given ended before the test could start in it.
#[derive(Debug, thiserror::Error)]
#[error("the browser session ended before the test started")]
struct SessionEnded;

/// Run `suite`, recording its wall time if it is named.
fn run_suite<'r, Context>(
    env: &'r Env<'_, Context>,
    suite: &'r Suite,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'r>>
where
    Context: Sync + ?Sized,
{
    Box::pin(async move {
        let start = Instant::now();
        // The futures only start running when the stream polls them, in order.
        let children: Vec<_> = suite
            .children
            .iter()
            .map(|child| run_node(env, child))
            .collect();
        futures_util::stream::iter(children)
            .buffer_unordered(suite.parallelism.max_parallel_tests().get())
            .collect::<()>()
            .await;
        if let Some(name) = &suite.name {
            env.results().suites.push((
                suite.id,
                SuiteRecord {
                    name: name.clone(),
                    duration: start.elapsed(),
                },
            ));
        }
    })
}

async fn run_node<Context>(env: &Env<'_, Context>, node: &Node)
where
    Context: Sync + ?Sized,
{
    match node {
        // Tests not started because of fail-fast or a cancellation are not recorded.
        Node::Test(index) if env.pool.may_start(env.test(*index).placement.run_always) => {
            run_test(env, *index).await;
        }
        Node::Test(_) => {}
        Node::Suite(suite) => run_suite(env, suite).await,
    }
}

/// Run the test at `index` in a session of the pool (or one created for it) and return once its
/// body finished. Its session quits or is reset, and its record is completed, in the background.
async fn run_test<Context>(env: &Env<'_, Context>, index: usize)
where
    Context: Sync + ?Sized,
{
    let test = env.test(index);
    let prepared = match &test.prepared {
        Ok(prepared) => prepared,
        Err(panic_message) => {
            let report = Report::new(BrowserTestError::Panic {
                test_name: test.name.clone(),
                message: panic_message.clone(),
            });
            env.finish(test.record(TestOutcome::Panicked, None, None), Err(report));
            return;
        }
    };

    let run_always = test.placement.run_always;
    let requested = Instant::now();
    let pooled = prepared.pooled;
    let ticket = if pooled {
        // Tests not started because of fail-fast or a cancellation are not recorded.
        let Some(ticket) = env.pool.take(prepared.fresh_session, run_always).await else {
            return;
        };
        ticket
    } else {
        let Some(ticket) = env
            .pool
            .dedicated(prepared.element_query_wait, run_always)
            .await
        else {
            return;
        };
        ticket
    };
    let wait = requested.elapsed();
    if !env.pool.may_start(run_always) {
        // A test failed, or the run was cancelled, while this one waited for its session: it
        // doesn't start, like the tests after it. Its session quits.
        if pooled && ticket.is_ok() {
            env.pool.test_finished();
        }
        return;
    }
    let (report, preparation) = match ticket {
        Ok(ticket) => {
            let (body_done_tx, body_done_rx) = oneshot::channel();
            let job = Job {
                test: index,
                wait,
                body_done: body_done_tx,
            };
            let preparation = ticket.preparation;
            let started = Instant::now();
            if ticket.job.send(job).is_ok() {
                if body_done_rx.await.is_err() {
                    // The worker ended without finishing the test, which only a cancelled run
                    // does: it drops its workers, before they take the test off the board.
                    env.board.clear(Activity::Test(index));
                    if pooled {
                        env.pool.test_finished();
                    }
                    let record = test.record(
                        TestOutcome::Cancelled,
                        Some(SessionTiming { preparation, wait }),
                        Some(started.elapsed()),
                    );
                    env.finish(record, Ok(()));
                }
                return;
            }
            if pooled {
                env.pool.test_finished();
            }
            (
                Report::<SessionEnded>::new(SessionEnded).into_dynamic(),
                preparation,
            )
        }
        Err(failure) => (failure.report, failure.preparation),
    };

    let session = SessionTiming { preparation, wait };
    let record = test.record(TestOutcome::Failed, Some(session), None);
    let report = report.context(BrowserTestError::PrepareSession {
        test_name: test.name.clone(),
    });
    env.finish(record, Err(report));
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    mod default_spare_sessions {
        use super::*;

        #[test]
        fn one_per_running_test_or_per_eight_with_reuse() {
            assert_that!(default_spare_sessions(8, SessionReuse::disabled())).is_equal_to(8);
            assert_that!(default_spare_sessions(8, SessionReuse::enabled())).is_equal_to(1);
            assert_that!(default_spare_sessions(9, SessionReuse::enabled())).is_equal_to(2);
            assert_that!(default_spare_sessions(1, SessionReuse::enabled())).is_equal_to(1);
        }
    }

    /// A run's future can be spawned.
    #[test]
    fn execute_tests_is_send() {
        fn send<'a>(
            config: &'a ExecutionConfig<'a>,
            tests: TestSuite<()>,
        ) -> impl Future<Output = Execution> + Send + 'a {
            execute_tests(config, &(), tests)
        }
        let _ = send;
    }

    /// Runs with fake sessions. Time is paused, so that sleeps end once nothing else can happen.
    /// Every run ends with the pool's counts back at zero ([`SessionPool::debug_assert_settled`]).
    mod execute {
        use std::time::Duration;

        use super::*;
        use crate::{
            ElementQueryWait, Parallelism, TestSettings,
            execution::fake_sessions::{FakeReset, FakeSessions, FakeTest},
            test_support::StubTest,
        };

        fn settings(cancellation: &CancellationToken) -> RunSettings<'_> {
            RunSettings {
                test_defaults: TestDefaults::new(),
                cancellation,
                failure_policy: FailurePolicy::RunAll,
                progress_warnings: ProgressWarnings::disabled(),
                spare_sessions: Some(0),
                session_reuse: SessionReuse::disabled(),
                driver_output: None,
            }
        }

        /// [`execute`], failing rather than hanging if the run never ends.
        async fn run(
            sessions: &FakeSessions,
            config: RunSettings<'_>,
            tests: TestSuite<()>,
        ) -> Execution {
            let run = execute(sessions, config, &(), tests);
            tokio::time::timeout(Duration::from_secs(3600), run)
                .await
                .expect("the run should end")
        }

        fn outcomes(execution: &Execution) -> Vec<(&str, TestOutcome)> {
            execution
                .records
                .iter()
                .map(|record| (record.name.as_str(), record.outcome))
                .collect()
        }

        fn sequential(names: &[&'static str]) -> TestSuite<()> {
            names.iter().fold(TestSuite::sequential(), |suite, name| {
                suite.with(StubTest::new(name))
            })
        }

        /// A test with an element query wait of its own, so that it gets a dedicated session.
        fn dedicated(name: &'static str) -> StubTest {
            StubTest::new(name).with_settings(TestSettings::new().with_element_query_wait(
                ElementQueryWait::new(Duration::from_secs(1), Duration::from_millis(100)),
            ))
        }

        #[tokio::test(start_paused = true)]
        async fn a_spare_that_could_not_be_created_fails_no_test() {
            let cancellation = CancellationToken::new();
            // Session 0 is the first spare, which the first test takes. Session 1, the next spare,
            // can't be created while that test runs: it is created again.
            let sessions = FakeSessions::new(&cancellation).failing_creation(1);
            let config = RunSettings {
                spare_sessions: Some(1),
                ..settings(&cancellation)
            };

            let execution = run(&sessions, config, sequential(&["a", "b", "c"])).await;

            assert_that!(execution.result).is_ok();
            assert_that!(outcomes(&execution)).contains_exactly([
                ("a", TestOutcome::Passed),
                ("b", TestOutcome::Passed),
                ("c", TestOutcome::Passed),
            ]);
            assert_that!(sessions.events()).contains("fail 1".to_owned());
        }

        #[tokio::test(start_paused = true)]
        async fn a_test_fails_when_its_session_could_not_be_created() {
            let cancellation = CancellationToken::new();
            let sessions = FakeSessions::new(&cancellation).failing_creation(0);

            let execution = run(&sessions, settings(&cancellation), sequential(&["a", "b"])).await;

            assert_that!(outcomes(&execution))
                .contains_exactly([("a", TestOutcome::Failed), ("b", TestOutcome::Passed)]);
            let error = execution.result.expect_err("a fails");
            assert_that!(format!("{error:?}"))
                .contains("PrepareSession { test_name: \"a\" }")
                .contains("session 0 could not be created");
        }

        #[tokio::test(start_paused = true)]
        async fn a_failure_to_quit_its_session_fails_a_passing_test() {
            let cancellation = CancellationToken::new();
            let sessions = FakeSessions::new(&cancellation).failing_quit(0);

            let execution = run(&sessions, settings(&cancellation), sequential(&["a"])).await;

            assert_that!(outcomes(&execution)).contains_exactly([("a", TestOutcome::Failed)]);
            let error = execution.result.expect_err("quitting fails");
            assert_that!(format!("{error:?}"))
                .contains("RunTest { test_name: \"a\" }")
                .contains("session 0 could not be quit");
        }

        #[tokio::test(start_paused = true)]
        async fn a_session_whose_reset_takes_too_long_quits() {
            let cancellation = CancellationToken::new();
            let sessions = FakeSessions::new(&cancellation).with_reset(FakeReset::Hang);
            let config = RunSettings {
                session_reuse: SessionReuse::enabled(),
                ..settings(&cancellation)
            };

            let execution = run(&sessions, config, sequential(&["a", "b"])).await;

            assert_that!(outcomes(&execution))
                .contains_exactly([("a", TestOutcome::Passed), ("b", TestOutcome::Passed)]);
            // The next test waited for the reset, and got a new session once it timed out.
            assert_that!(sessions.events()).contains_exactly([
                "create 0", "start a", "end a", "reset 0", "quit 0", "create 1", "start b",
                "end b", "quit 1",
            ]);
        }

        #[tokio::test(start_paused = true)]
        async fn a_reset_session_runs_the_next_test() {
            let cancellation = CancellationToken::new();
            let sessions = FakeSessions::new(&cancellation);
            let config = RunSettings {
                session_reuse: SessionReuse::enabled(),
                ..settings(&cancellation)
            };

            let execution = run(&sessions, config, sequential(&["a", "b"])).await;

            assert_that!(execution.result).is_ok();
            assert_that!(sessions.events()).contains_exactly([
                "create 0", "start a", "end a", "reset 0", "start b", "end b", "quit 0",
            ]);
        }

        #[tokio::test(start_paused = true)]
        async fn cancelling_the_run_during_a_reset_ends_it() {
            let cancellation = CancellationToken::new();
            let sessions = FakeSessions::new(&cancellation).with_reset(FakeReset::Cancel);
            let config = RunSettings {
                session_reuse: SessionReuse::enabled(),
                ..settings(&cancellation)
            };

            let execution = run(&sessions, config, sequential(&["a", "b"])).await;

            assert_that!(outcomes(&execution)).contains_exactly([("a", TestOutcome::Passed)]);
            assert_that!(execution.not_started).is_equal_to(1);
        }

        #[tokio::test(start_paused = true)]
        async fn cancelling_the_run_ends_the_wait_for_a_dedicated_session() {
            let cancellation = CancellationToken::new();
            // The pool session of "cancel" is created first, the dedicated one of "a" takes long.
            let sessions = FakeSessions::new(&cancellation)
                .with_creation_time(1, Duration::from_secs(60))
                .with_test("cancel", FakeTest::Cancel(Duration::from_secs(1)));
            let tests = TestSuite::parallel(Parallelism::parallel(2))
                .with(StubTest::new("cancel"))
                .with(dedicated("a"));

            let execution = run(&sessions, settings(&cancellation), tests).await;

            assert_that!(outcomes(&execution))
                .contains_exactly([("cancel", TestOutcome::Cancelled)]);
            assert_that!(execution.not_started).is_equal_to(1);
        }

        #[tokio::test(start_paused = true)]
        async fn a_failure_ends_the_wait_for_a_dedicated_session_with_fail_fast() {
            let cancellation = CancellationToken::new();
            let sessions = FakeSessions::new(&cancellation)
                .with_creation_time(1, Duration::from_secs(60))
                .with_test("fail", FakeTest::Fail);
            let tests = TestSuite::parallel(Parallelism::parallel(2))
                .with(StubTest::new("fail"))
                .with(dedicated("a"));
            let config = RunSettings {
                failure_policy: FailurePolicy::FailFast,
                ..settings(&cancellation)
            };

            let execution = run(&sessions, config, tests).await;

            assert_that!(outcomes(&execution)).contains_exactly([("fail", TestOutcome::Failed)]);
            assert_that!(execution.not_started).is_equal_to(1);
        }

        #[tokio::test(start_paused = true)]
        async fn tests_start_in_order_when_their_sessions_arrive_out_of_order() {
            let cancellation = CancellationToken::new();
            // The session created for "a" takes longer than the one created for "b".
            let sessions = FakeSessions::new(&cancellation)
                .with_creation_time(0, Duration::from_secs(5))
                .with_test("a", FakeTest::Pass(Duration::from_secs(10)));
            let tests = TestSuite::parallel(Parallelism::parallel(2))
                .with(StubTest::new("a"))
                .with(StubTest::new("b"));

            let execution = run(&sessions, settings(&cancellation), tests).await;

            assert_that!(execution.result).is_ok();
            let starts: Vec<_> = sessions
                .events()
                .into_iter()
                .filter(|event| event.starts_with("start"))
                .collect();
            assert_that!(starts).contains_exactly(["start a", "start b"]);
        }
    }
}

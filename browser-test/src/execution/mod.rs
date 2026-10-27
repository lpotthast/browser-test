//! Executes a tree of browser tests.
//!
//! Suites run recursively: a suite starts its entries in order, at most its parallelism at once.
//! Every test runs in a browser session taken from the [`SessionPool`], which keeps spare
//! sessions ready so that a starting test rarely waits for a browser to start. A session's
//! lifetime is bound to `chrome-for-testing-manager`'s scoped session API, so each session is
//! owned by a worker future that creates it, offers it to the pool, runs the test it is assigned,
//! and quits it. With [`SessionReuse`] enabled, a worker resets its session after the test and
//! offers it to the pool again instead, as long as upcoming tests need sessions. Worker futures are
//! driven by [`drive_workers`]. Everything crossing between them and the executor is plain data
//! (test indices, channels), never a borrow of the run's state.
//!
//! - [`plan`] numbers the tests and completes their settings with the runner's.
//! - [`session_pool`] decides when sessions are created, reset, or quit.
//! - [`worker`] owns each session, and [`test_run`] runs a test in it, within the test's time
//!   limit, and turns what happened into its record and report.

use std::{
    future::Future,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrome_for_testing_manager::{CancellationToken, ChromeForTesting};
use futures_util::StreamExt as _;
use rootcause::Report;
use thirtyfour::{ChromeCapabilities, error::WebDriverResult};
use tokio::sync::{mpsc, oneshot};

mod plan;
mod session_pool;
mod test_run;
mod worker;

use self::{
    plan::{Node, QueuedTest, Suite, plan},
    session_pool::{CreationFailure, Delivery, SessionPool, Upcoming},
    worker::{Job, drive_workers},
};
use crate::{
    BrowserTestError, FailurePolicy, ProgressWarnings, SessionReuse, TestSuite,
    profile::RunProfiles,
    progress::{Activity, ProgressBoard},
    report::{
        BrowserTestRecord, FormatDuration, SessionPreparation, SessionTiming, SuiteRecord,
        TestOutcome, timing_breakdown,
    },
    scheduler::BrowserTestFailures,
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
}

/// What executing a test tree produced.
pub(crate) struct Execution {
    /// A record per started test, in test order.
    pub(crate) records: Vec<BrowserTestRecord>,
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
    let (root, prepared) = plan(tests, config.test_defaults);
    let max_concurrency = root.max_concurrency();
    let default_wait = config.test_defaults.element_query_wait;
    let pooled = |fresh: bool, only_run_always: bool| {
        prepared
            .iter()
            .filter(|test| {
                matches!(&test.prepared, Ok(prepared)
                    if prepared.uses_pool(default_wait)
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
        keep_starting: AtomicBool::new(true),
        failures: Mutex::default(),
        records: Mutex::default(),
        suites: Mutex::default(),
        released_session_teardown: Mutex::default(),
        board: ProgressBoard::default(),
        pool: SessionPool::new(
            max_concurrency,
            config
                .spare_sessions
                .unwrap_or_else(|| default_spare_sessions(max_concurrency, config.session_reuse)),
            upcoming,
            upcoming_always,
            default_wait,
            requests_tx,
            config.cancellation.clone(),
        ),
    };

    env.pool.replenish(true);
    let execution = async {
        run_suite(&env, &root).await;
        env.pool.close();
    };
    let run = futures_util::future::join(execution, drive_workers(&env, requests_rx));
    // The watchdog never finishes on its own. It is dropped once the run is done.
    let watchdog = env.board.watch(config.progress_warnings);
    futures_util::future::select(pin!(run), pin!(watchdog)).await;

    let Env {
        tests,
        failures,
        records,
        suites,
        released_session_teardown,
        ..
    } = env;
    let mut records = records.into_inner().unwrap_or_else(PoisonError::into_inner);
    let not_started = tests.len() - records.len();
    records.sort_by_key(|record| record.index);
    let mut suites = suites.into_inner().unwrap_or_else(PoisonError::into_inner);
    suites.sort_by_key(|(id, _)| *id);
    let failures = failures
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    let result = failures.into_result();
    Execution {
        records,
        suites: suites.into_iter().map(|(_, suite)| suite).collect(),
        released_session_teardown: released_session_teardown
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner),
        not_started,
        result,
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
    config: &'c ExecutionConfig<'c>,
    context: &'c Context,
    tests: Vec<QueuedTest<Context>>,
    keep_starting: AtomicBool,
    failures: Mutex<BrowserTestFailures>,
    records: Mutex<Vec<BrowserTestRecord>>,
    suites: Mutex<Vec<(usize, SuiteRecord)>>,
    /// Time spent quitting sessions without a test.
    released_session_teardown: Mutex<Duration>,
    board: ProgressBoard,
    pool: SessionPool,
}

impl<Context> Env<'_, Context>
where
    Context: Sync + ?Sized,
{
    /// The test at `index`, which a session was assigned. Only ready tests are.
    fn assigned_test(&self, index: usize) -> &QueuedTest<Context> {
        &self.tests[index]
    }

    /// Whether further tests start: the run neither stopped on a failure nor was cancelled.
    fn keep_starting(&self) -> bool {
        self.keep_starting.load(Ordering::SeqCst) && !self.is_cancelled()
    }

    fn is_cancelled(&self) -> bool {
        self.config.cancellation.is_cancelled()
    }

    /// Whether a test starts: unless the run stopped on a failure, or, also for tests that run
    /// always, was cancelled.
    fn may_start(&self, run_always: bool) -> bool {
        self.keep_starting() || (run_always && !self.is_cancelled())
    }

    /// With [`FailurePolicy::FailFast`], start no further tests (after a failure).
    fn stop_starting_on_fail_fast(&self) {
        if self.config.failure_policy == FailurePolicy::FailFast {
            self.keep_starting.store(false, Ordering::SeqCst);
            self.pool.stopped();
        }
    }

    /// Record a finished test.
    fn finish(&self, record: BrowserTestRecord, result: Result<(), Report<BrowserTestError>>) {
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
        if let Err(report) = result {
            self.stop_starting_on_fail_fast();
            self.failures
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(record.index, report);
        }
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(record);
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
            env.suites
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((
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
        Node::Test(index) if env.may_start(env.tests[*index].placement.run_always) => {
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
    let test = &env.tests[index];
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
    let pooled = prepared.uses_pool(env.pool.default_wait);
    let ticket = if pooled {
        // Tests not started because of fail-fast or a cancellation are not recorded.
        let Some(ticket) = env
            .pool
            .take(prepared.fresh_session, run_always, || env.keep_starting())
            .await
        else {
            return;
        };
        ticket
    } else {
        env.pool.dedicated(prepared.element_query_wait).await
    };
    let wait = requested.elapsed();
    if !env.may_start(run_always) {
        // A test failed, or the run was cancelled, while this one waited for its session: it
        // doesn't start, like the tests after it. Its session quits.
        if pooled && ticket.is_ok() {
            env.pool.test_finished();
        }
        return;
    }
    let failure = match ticket {
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
            CreationFailure {
                report: rootcause::report!("the browser session ended before the test started"),
                preparation,
            }
        }
        Err(failure) => failure,
    };

    let session = SessionTiming {
        preparation: failure.preparation,
        wait,
    };
    let record = test.record(TestOutcome::Failed, Some(session), None);
    let report = failure.report.context(BrowserTestError::PrepareSession {
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
}

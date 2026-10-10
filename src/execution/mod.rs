//! Executes a tree of browser tests.
//!
//! Groups run recursively: a group starts its entries in order, at most its parallelism at once.
//! Every test runs in a browser session taken from the [`SessionPool`], which keeps spare
//! sessions ready so that a starting test rarely waits for a browser to start. A session's
//! lifetime is bound to `chrome-for-testing-manager`'s scoped session API, so each session is
//! owned by a worker future that creates it, offers it to the pool, runs the test it is assigned,
//! and quits it. With [`SessionReuse`] enabled, a worker resets its session after the test and
//! offers it to the pool again instead, as long as upcoming tests need sessions. Worker futures are
//! driven by [`drive_workers`]. Everything crossing between them and the executor is plain data
//! (test indices, channels), never a borrow of the run's state.

use std::{
    any::Any,
    collections::BTreeMap,
    future::Future,
    panic::AssertUnwindSafe,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrome_for_testing_manager::{CancellationToken, ChromeForTesting, Session};
use futures_util::{FutureExt as _, StreamExt as _, stream::FuturesUnordered};
use rootcause::Report;
use thirtyfour::{
    ChromeCapabilities, ChromiumLikeCapabilities,
    error::{WebDriverError, WebDriverResult},
};
use tokio::sync::{mpsc, oneshot};
use tracing::Instrument as _;

mod session_pool;

use self::session_pool::{CreationFailure, Delivery, SessionPool, Ticket, Upcoming, WorkerRequest};
use crate::{
    BrowserTest, BrowserTestError, BrowserTests, ElementQueryWait, FailurePolicy, Parallelism,
    ProgressWarnings, SessionReuse, SessionSettings, Timeouts,
    failure_report::{PanicDetails, PanicLocation, RecentSteps, without_own_location},
    profile::RunProfiles,
    progress::{Activity, Phase, ProgressBoard},
    report::{
        BrowserTestRecord, FormatDuration, GroupRecord, SessionPreparation, SessionTiming,
        StepStats, TestOutcome, timing_breakdown,
    },
    scheduler::BrowserTestFailures,
    session_reuse::{ResetError, SessionBaseline, configure_reusable_session},
    step::StepRecorder,
    test_case::BrowserTestEntry,
};

pub(crate) type ChromeCapabilitiesSetup =
    dyn Fn(&mut ChromeCapabilities) -> WebDriverResult<()> + Send + Sync + 'static;

/// Everything the runner configured for executing tests.
pub(crate) struct ExecutionConfig<'a> {
    pub(crate) chrome: &'a ChromeForTesting,
    pub(crate) visible: bool,
    /// The runner's timeouts and element query wait, which tests override.
    pub(crate) session_defaults: SessionSettings,
    pub(crate) chrome_capabilities_setups: &'a [Arc<ChromeCapabilitiesSetup>],
    /// Where sessions get their profiles.
    pub(crate) profiles: &'a RunProfiles,
    /// Cancelled when the run is cancelled: no further tests start, and running ones are cancelled.
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
    /// The timing of every named group, in definition order.
    pub(crate) groups: Vec<GroupRecord>,
    pub(crate) result: Result<(), Report<BrowserTestError>>,
}

/// Run every test of `tests`.
pub(crate) async fn execute_tests<Context, TestError>(
    config: &ExecutionConfig<'_>,
    context: &Context,
    tests: BrowserTests<Context, TestError>,
) -> Execution
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let mut prepared = Vec::new();
    let mut next_group = 0;
    let root = prepare_group(
        tests,
        None,
        &mut prepared,
        &mut next_group,
        config.session_defaults,
    );
    let max_concurrency = root.max_concurrency();
    let default_wait = config.session_defaults.element_query_wait();
    let pooled = |fresh: bool| {
        prepared
            .iter()
            .filter(|test| {
                matches!(test, QueuedTest::Ready(test)
                    if test.uses_pool(default_wait) && test.fresh_session == fresh)
            })
            .count()
    };
    let upcoming = Upcoming {
        any: pooled(false),
        fresh: pooled(true),
    };
    let (requests_tx, requests_rx) = mpsc::unbounded_channel();
    let env = Env {
        config,
        context,
        tests: prepared,
        keep_starting: AtomicBool::new(true),
        failures: Mutex::default(),
        records: Mutex::default(),
        groups: Mutex::default(),
        board: ProgressBoard::default(),
        pool: SessionPool::new(
            max_concurrency,
            config
                .spare_sessions
                .unwrap_or_else(|| default_spare_sessions(max_concurrency, config.session_reuse)),
            upcoming,
            default_wait,
            requests_tx,
            config.cancellation.clone(),
        ),
    };

    env.pool.replenish(true);
    let execution = async {
        run_group(&env, &root, false).await;
        env.pool.close();
    };
    let run = futures_util::future::join(execution, drive_workers(&env, requests_rx));
    // The watchdog never finishes on its own. It is dropped once the run is done.
    let watchdog = env.board.watch(config.progress_warnings);
    futures_util::future::select(pin!(run), pin!(watchdog)).await;

    let Env {
        failures,
        records,
        groups,
        ..
    } = env;
    let mut records = records.into_inner().unwrap_or_else(PoisonError::into_inner);
    records.sort_by_key(|record| record.index);
    let mut groups = groups.into_inner().unwrap_or_else(PoisonError::into_inner);
    groups.sort_by_key(|(id, _)| *id);
    let failures = failures
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    let result = if max_concurrency == 1 && config.failure_policy == FailurePolicy::FailFast {
        failures.into_first_result()
    } else {
        failures.into_result()
    };
    Execution {
        records,
        groups: groups.into_iter().map(|(_, group)| group).collect(),
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

/// A test whose metadata (name, session settings) was read successfully.
struct PreparedTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    index: usize,
    name: String,
    group: Option<String>,
    /// The test's timeouts, completed with the runner's.
    timeouts: Timeouts,
    element_query_wait: Option<ElementQueryWait>,
    /// Whether the test needs a session no test ran in (see [`SessionSettings::fresh_session`]).
    fresh_session: bool,
    test: Box<dyn BrowserTest<Context, TestError>>,
}

impl<Context, TestError> PreparedTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    /// Whether the test takes its session from the pool, rather than getting one created with
    /// its own settings.
    fn uses_pool(&self, default_wait: Option<ElementQueryWait>) -> bool {
        self.element_query_wait == default_wait
    }
}

enum QueuedTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    Ready(PreparedTest<Context, TestError>),
    /// Reading the test's metadata panicked. The test fails without running.
    Broken {
        index: usize,
        name: String,
        group: Option<String>,
        panic_message: String,
    },
}

/// A group of the test tree, with its tests referenced by index.
struct Group {
    /// Definition order, for sorting group records.
    id: usize,
    /// The group's name path, if the group is named.
    name: Option<String>,
    run_always: bool,
    parallelism: Parallelism,
    children: Vec<Node>,
}

enum Node {
    Test(usize),
    Group(Group),
}

impl Group {
    /// How many tests of this group can run at the same time.
    fn max_concurrency(&self) -> usize {
        let mut children: Vec<usize> = self
            .children
            .iter()
            .map(|child| match child {
                Node::Test(_) => 1,
                Node::Group(group) => group.max_concurrency(),
            })
            .collect();
        children.sort_unstable_by(|a, b| b.cmp(a));
        children
            .into_iter()
            .take(self.parallelism.max_parallel_tests().get())
            .sum::<usize>()
            .max(1)
    }
}

/// Number the tests of `tests` in definition order into `prepared` and build the tree.
fn prepare_group<Context, TestError>(
    tests: BrowserTests<Context, TestError>,
    parent_path: Option<&str>,
    prepared: &mut Vec<QueuedTest<Context, TestError>>,
    next_group: &mut usize,
    session_defaults: SessionSettings,
) -> Group
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let BrowserTests {
        parallelism,
        name,
        run_always,
        entries,
    } = tests;
    let named = name.is_some();
    let path = match (parent_path, name) {
        (Some(parent), Some(name)) => Some(format!("{parent} / {name}")),
        (None, Some(name)) => Some(name),
        (parent, None) => parent.map(str::to_owned),
    };
    let id = *next_group;
    *next_group += 1;
    let mut children = Vec::new();
    for entry in entries {
        match entry {
            BrowserTestEntry::Test(test) => {
                let index = prepared.len();
                prepared.push(prepare_test(index, test, path.clone(), session_defaults));
                children.push(Node::Test(index));
            }
            BrowserTestEntry::Group(group) => children.push(Node::Group(prepare_group(
                group,
                path.as_deref(),
                prepared,
                next_group,
                session_defaults,
            ))),
            BrowserTestEntry::TestGroup(group) => {
                let group_path = match path.as_deref() {
                    Some(parent) => format!("{parent} / {}", group.name),
                    None => group.name,
                };
                for test in group.tests {
                    let index = prepared.len();
                    prepared.push(prepare_test(
                        index,
                        test,
                        Some(group_path.clone()),
                        session_defaults,
                    ));
                    children.push(Node::Test(index));
                }
            }
        }
    }
    Group {
        id,
        name: if named { path } else { None },
        run_always,
        parallelism,
        children,
    }
}

fn prepare_test<Context, TestError>(
    index: usize,
    test: Box<dyn BrowserTest<Context, TestError>>,
    group: Option<String>,
    session_defaults: SessionSettings,
) -> QueuedTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    // Used in the panic report. We need a fallback for the case that `.name()` panics.
    let mut name = format!("unnamed test at index {index}");
    let metadata = std::panic::catch_unwind(AssertUnwindSafe(|| {
        name = test.name().into_owned();
        let settings = test.session_settings();
        (
            settings.timeouts().or(session_defaults.timeouts()),
            settings
                .element_query_wait()
                .or(session_defaults.element_query_wait()),
            settings.fresh_session(),
        )
    }));
    match metadata {
        Ok((timeouts, element_query_wait, fresh_session)) => QueuedTest::Ready(PreparedTest {
            index,
            name,
            group,
            timeouts,
            element_query_wait,
            fresh_session,
            test,
        }),
        Err(payload) => {
            let panic_message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{name}' panicked: {panic_message}");
            QueuedTest::Broken {
                index,
                name,
                group,
                panic_message,
            }
        }
    }
}

/// State of one run, shared by the executor and the session workers.
struct Env<'c, Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    config: &'c ExecutionConfig<'c>,
    context: &'c Context,
    tests: Vec<QueuedTest<Context, TestError>>,
    keep_starting: AtomicBool,
    failures: Mutex<BrowserTestFailures>,
    records: Mutex<Vec<BrowserTestRecord>>,
    groups: Mutex<Vec<(usize, GroupRecord)>>,
    board: ProgressBoard,
    pool: SessionPool,
}

impl<Context, TestError> Env<'_, Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    /// The test at `index`, which a session was assigned. Only ready tests are.
    fn assigned_test(&self, index: usize) -> &PreparedTest<Context, TestError> {
        match &self.tests[index] {
            QueuedTest::Ready(test) => test,
            QueuedTest::Broken { .. } => unreachable!("only ready tests are assigned to sessions"),
        }
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

    /// Record the finished test of `run`, whose session ended with `session_result` after
    /// `teardown` (`None`: the session runs further tests).
    fn finish_job(
        &self,
        mut run: JobRun,
        session_result: Result<(), Report<chrome_for_testing_manager::ChromeForTestingError>>,
        teardown: Option<Duration>,
    ) {
        let test = self.assigned_test(run.index);
        let (outcome, result) = test_outcome(&test.name, &mut run, session_result);
        let record = BrowserTestRecord {
            index: run.index,
            name: test.name.clone(),
            outcome,
            group: test.group.clone(),
            session: Some(SessionTiming {
                preparation: run.preparation,
                wait: run.wait,
            }),
            body: run.body,
            teardown,
            steps: run.steps,
        };
        self.finish(record, result);
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

/// Run `group`, recording its wall time if it is named.
fn run_group<'r, Context, TestError>(
    env: &'r Env<'_, Context, TestError>,
    group: &'r Group,
    inherited_run_always: bool,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'r>>
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    Box::pin(async move {
        let run_always = inherited_run_always || group.run_always;
        let start = Instant::now();
        // The futures only start running when the stream polls them, in order.
        let children: Vec<_> = group
            .children
            .iter()
            .map(|child| run_node(env, child, run_always))
            .collect();
        futures_util::stream::iter(children)
            .buffer_unordered(group.parallelism.max_parallel_tests().get())
            .collect::<()>()
            .await;
        if let Some(name) = &group.name {
            env.groups
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((
                    group.id,
                    GroupRecord {
                        name: name.clone(),
                        duration: start.elapsed(),
                    },
                ));
        }
    })
}

async fn run_node<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    node: &Node,
    run_always: bool,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    match node {
        // Tests not started because of fail-fast or a cancellation are not recorded.
        Node::Test(index) if env.may_start(run_always) => run_test(env, *index, run_always).await,
        Node::Test(_) => {}
        Node::Group(group) => run_group(env, group, run_always).await,
    }
}

/// Run the test at `index` in a session of the pool (or one created for it) and return once its
/// body finished. Its session quits or is reset, and its record is completed, in the background.
async fn run_test<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    index: usize,
    run_always: bool,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let test = match &env.tests[index] {
        QueuedTest::Ready(test) => test,
        QueuedTest::Broken {
            index,
            name,
            group,
            panic_message,
        } => {
            let report = Report::new(BrowserTestError::Panic {
                test_name: name.clone(),
                message: panic_message.clone(),
            });
            let record =
                record_without_body(*index, name, group.as_deref(), TestOutcome::Panicked, None);
            env.finish(record, Err(report));
            return;
        }
    };

    let requested = Instant::now();
    let pooled = test.uses_pool(env.pool.default_wait);
    let ticket = if pooled {
        // Tests not started because of a cancellation are not recorded.
        let Some(ticket) = env.pool.take(test.fresh_session, env.keep_starting()).await else {
            return;
        };
        ticket
    } else {
        env.pool.dedicated(test.element_query_wait).await
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
            match ticket.job.send(job) {
                Ok(()) => {
                    // An error means the worker ended without running the body. It recorded
                    // the test itself.
                    let _ = body_done_rx.await;
                    return;
                }
                Err(_) => CreationFailure {
                    report: rootcause::report!("the browser session ended before the test started"),
                    preparation: ticket.preparation,
                },
            }
        }
        Err(failure) => failure,
    };

    let session = SessionTiming {
        preparation: failure.preparation,
        wait,
    };
    let record = record_without_body(
        index,
        &test.name,
        test.group.as_deref(),
        TestOutcome::Failed,
        Some(session),
    );
    let report = failure.report.context(BrowserTestError::RunTest {
        test_name: test.name.clone(),
    });
    env.finish(record, Err(report));
}

/// The record of a test that failed before its body ran.
fn record_without_body(
    index: usize,
    name: &str,
    group: Option<&str>,
    outcome: TestOutcome,
    session: Option<SessionTiming>,
) -> BrowserTestRecord {
    BrowserTestRecord {
        index,
        name: name.to_owned(),
        outcome,
        group: group.map(str::to_owned),
        session,
        body: None,
        teardown: None,
        steps: BTreeMap::new(),
    }
}

/// A test assigned to a session.
struct Job {
    test: usize,
    /// How long the test waited for its session.
    wait: Duration,
    /// Completed once the test body finished, before the session quits or is reset.
    body_done: oneshot::Sender<()>,
}

/// Drive the session workers until the pool closed and every worker finished.
async fn drive_workers<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    mut requests: mpsc::UnboundedReceiver<Option<WorkerRequest>>,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let mut workers = FuturesUnordered::new();
    let mut closing = false;
    loop {
        if closing && workers.is_empty() {
            return;
        }
        tokio::select! {
            request = requests.recv(), if !closing => match request {
                Some(Some(request)) => workers.push(session_worker(env, request)),
                Some(None) | None => closing = true,
            },
            Some(()) = workers.next(), if !workers.is_empty() => {}
        }
    }
}

/// What happened while a session ran its test.
struct JobRun {
    index: usize,
    preparation: SessionPreparation,
    wait: Duration,
    body: Option<Duration>,
    steps: BTreeMap<String, StepStats>,
    /// The error the test returned.
    test_error: Option<Report>,
    /// Set if the test body panicked.
    panic_message: Option<String>,
    /// Where the test panicked, if the panic hook saw it.
    panic: Option<PanicDetails>,
    /// The test's last steps, for its failure report.
    recent_steps: Option<RecentSteps>,
    quit_start: Option<Instant>,
}

/// Create a session, offer it, run the test it is assigned, and quit it. With [`SessionReuse`],
/// reset the session after its test and offer it again, as long as upcoming tests need sessions.
async fn session_worker<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    request: WorkerRequest,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let pooled = matches!(request.delivery, Delivery::Pool);
    run_session(env, request).await;
    if pooled {
        env.pool.session_ended(env.keep_starting());
    }
}

/// The work of [`session_worker`].
async fn run_session<Context, TestError>(env: &Env<'_, Context, TestError>, request: WorkerRequest)
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let config = env.config;
    let activity = Activity::Session(request.session);
    env.board
        .enter(activity, "a browser session", Phase::CreatingSession);
    let creation_start = Instant::now();
    let profile = match config.profiles.create_session_profile() {
        Ok(profile) => profile,
        Err(error) => {
            env.board.clear(activity);
            let report = Report::<rootcause::markers::Dynamic>::from(error)
                .context("the Chrome profile of the session could not be created")
                .into_dynamic();
            env.fail_creation(request.delivery, report, creation_start);
            return;
        }
    };
    let pooled = matches!(request.delivery, Delivery::Pool);
    // Only pool sessions are reused: a dedicated session has settings of its own. With reuse
    // enabled, dedicated sessions are set up like reusable ones all the same, so that their tests
    // see the same browser.
    let reusable = config.session_reuse.is_enabled() && pooled;
    let mut delivery = Some(request.delivery);
    // The run of the session's last test, recorded once the session quit.
    let mut last_run = None;
    let session_result = config
        .chrome
        .session()
        .with_caps(|caps: &mut ChromeCapabilities| {
            configure_chrome_capabilities(caps, config.visible, config.chrome_capabilities_setups)?;
            if config.session_reuse.is_enabled() {
                configure_reusable_session(caps, config.session_reuse)?;
            }
            profile.configure(caps)
        })
        .with_cancellation(config.cancellation.clone())
        .with_config(|builder| match request.element_query_wait {
            Some(wait) => builder.poller(Arc::new(wait.into_thirtyfour_poller())),
            None => builder,
        })
        .run(async |session: &Session| {
            serve_tests(
                env,
                session,
                SessionStart {
                    activity,
                    creation_start,
                    pooled,
                    reusable,
                },
                &mut delivery,
                &mut last_run,
            )
            .await
        })
        .await;
    profile.remove().await;
    env.board.clear(activity);

    if let Some(delivery) = delivery {
        // The session could not be created, so it was never offered.
        let report = match session_result {
            Err(error) => error.into_dynamic(),
            Ok(()) => rootcause::report!("the browser session ended before it could be used"),
        };
        env.fail_creation(delivery, report, creation_start);
        return;
    }
    let Some(last_run) = last_run else {
        return;
    };

    let teardown = last_run.quit_start.map(|quit_start| quit_start.elapsed());
    env.board.clear(Activity::Test(last_run.index));
    if let Some(teardown) = teardown {
        let test = env.assigned_test(last_run.index);
        env.warn_if_slow_session(
            &format!("Quitting the session of browser test '{}'", test.name),
            teardown,
        );
    }
    env.finish_job(last_run, session_result, teardown);
}

/// How a worker's session started.
struct SessionStart {
    /// The pool session's activity on the progress board.
    activity: Activity,
    creation_start: Instant,
    /// Whether the session belongs to the pool (rather than to one test with settings of its own).
    pooled: bool,
    /// Whether the session may run further tests after its first one.
    reusable: bool,
}

/// Offer `session` through `delivery` (or, once it ran a test, to the pool), and run the tests it
/// is assigned: one, or with [`SessionReuse`] several, resetting the session in between. The run
/// of the last test is left in `last_run`, to be recorded once the session quit.
async fn serve_tests<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    session: &Session,
    start: SessionStart,
    delivery: &mut Option<Delivery>,
    last_run: &mut Option<JobRun>,
) -> Result<(), Report>
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let config = env.config;
    let SessionStart {
        activity,
        creation_start,
        pooled,
        reusable,
    } = start;
    if let Err(error) = focus_page(session).await {
        return Err(Report::<rootcause::markers::Dynamic>::from(error)
            .context("the page of the session could not be brought to the front")
            .into_dynamic());
    }
    // Prepared for dedicated sessions as well: it selects where the test runs.
    let mut baseline = if config.session_reuse.is_enabled() {
        match SessionBaseline::prepare(session, config.session_reuse.reset()).await {
            Ok(baseline) => Some(baseline),
            Err(error) => {
                tracing::warn!("A browser session can't be reused, it runs one test only: {error}");
                None
            }
        }
    } else {
        None
    };
    let creation = creation_start.elapsed();
    env.board.clear(activity);
    env.warn_if_slow_session("Creating a browser session", creation);

    let mut preparation = SessionPreparation::Created(creation);
    let mut tests_run = 0;
    loop {
        let (job_tx, job_rx) = oneshot::channel();
        let ticket = Ticket {
            job: job_tx,
            preparation,
        };
        match delivery.take() {
            Some(delivery) => env.pool.deliver(delivery, Ok(ticket)),
            // Dropped if the pool has enough sessions: the job channel closes, and the session
            // quits below.
            None => env.pool.deliver_reset(ticket, env.keep_starting()),
        }
        // An error means no test needs this session anymore.
        let Ok(job) = job_rx.await else {
            return Ok(());
        };
        tests_run += 1;
        let mut run = run_job(env, session, &job, preparation).await;
        if pooled {
            env.pool.test_finished();
        }
        let reuse = reusable
            && baseline.is_some()
            && config.session_reuse.allows_another_test(tests_run)
            && env.pool.claim_reset(env.keep_starting());
        // Let the next test start while this session quits or is reset.
        let _ = job.body_done.send(());
        let test = env.assigned_test(run.index);
        let subject = format!("browser test '{}'", test.name);
        let Some(baseline) = baseline.as_mut().filter(|_| reuse) else {
            env.board
                .enter(Activity::Test(run.index), subject, Phase::QuittingSession);
            run.quit_start = Some(Instant::now());
            *last_run = Some(run);
            // The test's own failure is kept in `run`, so that its report doesn't pass
            // through the session.
            return Ok(());
        };

        let index = run.index;
        env.finish_job(run, Ok(()), None);
        env.board
            .enter(Activity::Test(index), subject, Phase::ResettingSession);
        let reset_start = Instant::now();
        let reset = baseline.reset(session).await;
        let reset_duration = reset_start.elapsed();
        env.board.clear(Activity::Test(index));
        env.warn_if_slow_session(
            &format!("Resetting the session of browser test '{}'", test.name),
            reset_duration,
        );
        if let Err(error) = reset {
            match error {
                ResetError::DefaultContextUsed(reason) => tracing::debug!(
                    "The session of browser test '{}' quits instead of being reset: {reason}.",
                    test.name,
                ),
                ResetError::Failed(error) => tracing::warn!(
                    "Resetting the session of browser test '{}' failed, so it quits: {error}",
                    test.name,
                ),
            }
            env.pool.reset_failed(env.keep_starting());
            return Ok(());
        }
        preparation = SessionPreparation::Reset(reset_duration);
    }
}

/// Bring the page of a new session to the front.
///
/// `ChromeDriver` starts a session on a profile it did not create itself (browser-test names every
/// session's profile, see [`RunProfiles`]) with a page that lacks focus: `document.hasFocus()` is
/// `false`, and focusing an element from script moves `document.activeElement` without firing
/// `focus` or `focusin` events. Sessions on `ChromeDriver`'s own profiles start with a focused
/// page. This restores that behavior.
async fn focus_page(session: &Session) -> WebDriverResult<()> {
    session
        .cdp()
        .send_raw("Page.bringToFront", serde_json::json!({}))
        .await?;
    Ok(())
}

/// Run the test of `job` in `session`, prepared as `preparation`.
async fn run_job<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    session: &Session,
    job: &Job,
    preparation: SessionPreparation,
) -> JobRun
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let test = env.assigned_test(job.test);
    let mut run = JobRun {
        index: job.test,
        preparation,
        wait: job.wait,
        body: None,
        steps: BTreeMap::new(),
        test_error: None,
        panic_message: None,
        panic: None,
        recent_steps: None,
        quit_start: None,
    };
    let result = if test.timeouts.is_empty() {
        Ok(())
    } else {
        session
            .update_timeouts(test.timeouts.into_thirtyfour_timeout_configuration())
            .await
            .map_err(Report::<rootcause::markers::Dynamic>::from)
    };
    match result {
        Ok(()) => run_body(env, session, test, &mut run).await,
        Err(error) => {
            env.stop_starting_on_fail_fast();
            run.test_error = Some(error);
        }
    }
    run
}

/// The outcome of the test of `run`, whose session ended with `session_result`. A failure's report
/// says where the test failed ([`TestCodeFrames`](crate::failure_report::TestCodeFrames)) and what
/// it did before ([`RecentSteps`]).
fn test_outcome(
    name: &str,
    run: &mut JobRun,
    session_result: Result<(), Report<chrome_for_testing_manager::ChromeForTestingError>>,
) -> (TestOutcome, Result<(), Report<BrowserTestError>>) {
    let recent_steps = run.recent_steps.take();
    if let Some(message) = run.panic_message.take() {
        let mut report: Report<BrowserTestError> =
            without_own_location(Report::new(BrowserTestError::Panic {
                test_name: name.to_owned(),
                message,
            }));
        if let Some(panic) = run.panic.take() {
            if let Some(location) = panic.location {
                let outside_test_code = panic
                    .frames
                    .as_ref()
                    .is_some_and(|frames| !frames.starts_at(&location));
                report = report.attach_custom::<rootcause::handlers::Display, _>(PanicLocation {
                    location,
                    outside_test_code,
                });
            }
            if let Some(frames) = panic.frames {
                report = report.attach_custom::<rootcause::handlers::Display, _>(frames);
            }
        }
        return (TestOutcome::Panicked, Err(with_steps(report, recent_steps)));
    }
    let error = match (run.test_error.take(), session_result) {
        (Some(error), _) => error,
        (None, Err(error)) => error.into_dynamic(),
        (None, Ok(())) => return (TestOutcome::Passed, Ok(())),
    };
    let report = without_own_location(error.context(BrowserTestError::RunTest {
        test_name: name.to_owned(),
    }));
    (TestOutcome::Failed, Err(with_steps(report, recent_steps)))
}

/// `report` with the test's last steps attached.
fn with_steps(
    report: Report<BrowserTestError>,
    recent_steps: Option<RecentSteps>,
) -> Report<BrowserTestError> {
    match recent_steps {
        Some(steps) => report.attach_custom::<rootcause::handlers::Display, _>(steps),
        None => report,
    }
}

/// Run the body of `test`, recording its duration and steps into `run`.
///
/// A failure is kept in `run`: the test's error in `run.test_error`, a panic in `run.panic_message`
/// and `run.panic`.
async fn run_body<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    session: &Session,
    test: &PreparedTest<Context, TestError>,
    run: &mut JobRun,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let name = test.name.as_str();
    tracing::debug!("Executing browser test: {name}");
    env.board.enter(
        Activity::Test(test.index),
        format!("browser test '{name}'"),
        Phase::RunningTest,
    );
    let recorder = StepRecorder::new(env.config.progress_warnings.slow_step());
    let span = tracing::info_span!(
        "browser_test",
        test = %name,
        group = test.group.as_deref().unwrap_or_default(),
    );
    let start = Instant::now();
    // `run` is called in the block, so that a panic before it returns its future (in a
    // hand-written implementation) is caught as well.
    let body = async { test.test.run(session, env.context).await };
    let result = recorder
        .scope(AssertUnwindSafe(body).catch_unwind())
        .instrument(span)
        .await;
    run.body = Some(start.elapsed());
    run.steps = recorder.take();
    run.recent_steps = recorder.recent_steps();
    run.panic = recorder.take_panic();
    if !matches!(result, Ok(Ok(()))) {
        // Don't let further tests start while this session quits.
        env.stop_starting_on_fail_fast();
    }

    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => run.test_error = Some(error.into_dynamic()),
        Err(payload) => {
            let message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{name}' panicked: {message}");
            run.panic_message = Some(message);
        }
    }
}

fn configure_chrome_capabilities(
    caps: &mut ChromeCapabilities,
    visible: bool,
    chrome_capabilities_setups: &[Arc<ChromeCapabilitiesSetup>],
) -> WebDriverResult<()> {
    if visible {
        caps.unset_headless()?;
    }
    for setup in chrome_capabilities_setups {
        // A panic would unwind through the whole run: it fails the session instead.
        std::panic::catch_unwind(AssertUnwindSafe(|| setup(caps))).unwrap_or_else(|payload| {
            Err(WebDriverError::SessionCreateError(format!(
                "a Chrome capability setup panicked: {}",
                panic_payload_message(payload.as_ref())
            )))
        })?;
    }
    Ok(())
}

fn panic_payload_message(payload: &(dyn Any + Send + 'static)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "<non-string panic payload>".to_owned()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use assertr::prelude::*;
    use rootcause::{Report, markers::Dynamic};
    use thirtyfour::{BrowserCapabilitiesHelper, WebDriver};

    use super::*;

    #[test]
    fn logical_groups_share_parent_concurrency_and_retain_names() {
        async fn passes((): &()) -> Result<(), Report> {
            Ok(())
        }
        let tests = BrowserTests::parallel(Parallelism::parallel(2))
            .named("ui")
            .with_test_group(
                crate::TestGroup::new("one")
                    .with(passes.named("a"))
                    .with(passes.named("b")),
            )
            .with_test_group(
                crate::TestGroup::new("two")
                    .with(passes.named("c"))
                    .with(passes.named("d")),
            );
        let mut prepared = Vec::new();
        let root = prepare_group(tests, None, &mut prepared, &mut 0, SessionSettings::new());
        assert_eq!(root.max_concurrency(), 2);
        assert_eq!(root.children.len(), 4);
        assert!(
            root.children
                .iter()
                .all(|child| matches!(child, Node::Test(_)))
        );
        let metadata: Vec<_> = prepared
            .iter()
            .map(|test| match test {
                QueuedTest::Ready(test) => (test.index, test.name.as_str(), test.group.as_deref()),
                QueuedTest::Broken { .. } => panic!("metadata must be valid"),
            })
            .collect();
        assert_eq!(
            metadata,
            [
                (0, "a", Some("ui / one")),
                (1, "b", Some("ui / one")),
                (2, "c", Some("ui / two")),
                (3, "d", Some("ui / two")),
            ]
        );
    }

    mod prepare_test {
        use super::*;

        struct SettingsTest(SessionSettings);

        #[async_trait::async_trait]
        impl BrowserTest for SettingsTest {
            fn session_settings(&self) -> SessionSettings {
                self.0
            }

            async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
                Ok(())
            }
        }

        fn prepared(test: SessionSettings, runner: SessionSettings) -> PreparedTest<(), Dynamic> {
            match prepare_test(0, Box::new(SettingsTest(test)), None, runner) {
                QueuedTest::Ready(test) => test,
                QueuedTest::Broken { .. } => panic!("the settings are readable"),
            }
        }

        #[test]
        fn test_timeouts_override_the_runners_one_by_one() {
            let runner = SessionSettings::new().with_timeouts(
                Timeouts::new()
                    .with_script(Duration::from_secs(10))
                    .with_implicit_wait(Duration::ZERO),
            );
            let test = SessionSettings::new()
                .with_timeouts(Timeouts::new().with_script(Duration::from_secs(5)));

            assert_that!(prepared(test, runner).timeouts).is_equal_to(
                Timeouts::new()
                    .with_script(Duration::from_secs(5))
                    .with_implicit_wait(Duration::ZERO),
            );
            assert_that!(prepared(SessionSettings::new(), SessionSettings::new()).timeouts)
                .is_equal_to(Timeouts::new());
        }

        #[test]
        fn test_element_query_wait_overrides_the_runners() {
            let runner_wait =
                ElementQueryWait::new(Duration::from_secs(10), Duration::from_secs(1));
            let test_wait =
                ElementQueryWait::new(Duration::from_secs(5), Duration::from_millis(500));
            let runner = SessionSettings::new().with_element_query_wait(runner_wait);

            let overriding = prepared(
                SessionSettings::new().with_element_query_wait(test_wait),
                runner,
            );
            assert_that!(overriding.element_query_wait).is_equal_to(Some(test_wait));
            assert_that!(overriding.uses_pool(Some(runner_wait))).is_false();

            let inheriting = prepared(SessionSettings::new(), runner);
            assert_that!(inheriting.element_query_wait).is_equal_to(Some(runner_wait));
            assert_that!(inheriting.uses_pool(Some(runner_wait))).is_true();

            assert_that!(
                prepared(SessionSettings::new(), SessionSettings::new()).element_query_wait
            )
            .is_none();
        }

        #[test]
        fn fresh_session_is_the_tests_own() {
            assert_that!(
                prepared(
                    SessionSettings::new().with_fresh_session(true),
                    SessionSettings::new()
                )
                .fresh_session
            )
            .is_true();
        }
    }

    mod configure_chrome_capabilities {
        use super::*;

        #[test]
        fn applies_visible_mode_before_custom_setup() {
            let custom_setup_called = Arc::new(AtomicUsize::new(0));
            let custom_setup = {
                let custom_setup_called = Arc::clone(&custom_setup_called);
                Arc::new(move |caps: &mut ChromeCapabilities| {
                    assert_that!(caps.is_headless()).is_false();
                    custom_setup_called.fetch_add(1, Ordering::SeqCst);
                    caps.add_arg("--window-size=800,600")
                }) as Arc<ChromeCapabilitiesSetup>
            };
            let mut caps = ChromeCapabilities::new();
            caps.set_headless()
                .expect("setting headless should update capabilities");

            configure_chrome_capabilities(&mut caps, true, &[custom_setup])
                .expect("capability setup should succeed");

            assert_that!(custom_setup_called.load(Ordering::SeqCst)).is_equal_to(1);
            assert_that!(caps.is_headless()).is_false();
            assert_that!(caps.has_arg("--window-size=800,600")).is_true();
        }

        #[test]
        fn a_panicking_setup_is_an_error() {
            let panicking = Arc::new(|_: &mut ChromeCapabilities| -> WebDriverResult<()> {
                panic!("no window size")
            }) as Arc<ChromeCapabilitiesSetup>;
            let error =
                configure_chrome_capabilities(&mut ChromeCapabilities::new(), false, &[panicking])
                    .expect_err("a panicking setup should fail");

            assert_that!(error.to_string())
                .contains("a Chrome capability setup panicked: no window size");
        }
    }

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

    mod panic_payload_message {
        use super::*;

        #[test]
        fn handles_common_payload_types() {
            assert_that!(panic_payload_message(&"static")).is_equal_to("static");
            assert_that!(panic_payload_message(&"owned".to_owned())).is_equal_to("owned");
            assert_that!(panic_payload_message(&42usize)).is_equal_to("<non-string panic payload>");
        }
    }
}

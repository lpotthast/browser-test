//! Executes a tree of browser tests.
//!
//! Groups run recursively: a group starts its entries in order, at most its parallelism at once.
//! Every test runs in a fresh browser session taken from the [`SessionPool`], which keeps spare
//! sessions ready so that a starting test rarely waits for a browser to start. A session's
//! lifetime is bound to `chrome-for-testing-manager`'s scoped session API, so each session is
//! owned by a worker future that creates it, offers it to the pool, runs the test it is assigned,
//! and quits it. Worker futures are driven by [`drive_workers`]. Everything crossing between them
//! and the executor is plain data (test indices, channels), never a borrow of the run's state.

use std::{
    any::Any,
    collections::{BTreeMap, VecDeque},
    future::Future,
    panic::AssertUnwindSafe,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrome_for_testing_manager::{ChromeForTesting, Session};
use futures_util::{FutureExt as _, StreamExt as _, stream::FuturesUnordered};
use rootcause::Report;
use thirtyfour::{ChromeCapabilities, ChromiumLikeCapabilities, error::WebDriverResult};
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::Instrument as _;

use crate::progress::{Activity, Phase, ProgressBoard};
use crate::report::{
    BrowserTestRecord, FormatDuration, GroupRecord, SessionTiming, StepStats, TestOutcome,
    timing_breakdown,
};
use crate::scheduler::BrowserTestFailures;
use crate::step::StepRecorder;
use crate::test_case::BrowserTestEntry;
use crate::{
    BrowserTest, BrowserTestError, BrowserTests, ElementQueryWait, FailurePolicy, Parallelism,
    ProgressWarnings, Timeouts,
};

pub(crate) type ChromeCapabilitiesSetup =
    dyn Fn(&mut ChromeCapabilities) -> WebDriverResult<()> + Send + Sync + 'static;

/// Everything the runner configured for executing tests.
pub(crate) struct ExecutionConfig<'a> {
    pub(crate) chrome: &'a ChromeForTesting,
    pub(crate) visible: bool,
    pub(crate) timeouts: Option<&'a Timeouts>,
    pub(crate) element_query_wait: Option<&'a ElementQueryWait>,
    pub(crate) chrome_capabilities_setups: &'a [Arc<ChromeCapabilitiesSetup>],
    pub(crate) failure_policy: FailurePolicy,
    pub(crate) progress_warnings: ProgressWarnings,
    /// Spare sessions kept ready. `None`: one per test that can run at the same time.
    pub(crate) spare_sessions: Option<usize>,
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
    let root = prepare_group(tests, None, &mut prepared, &mut next_group, config);
    let max_concurrency = root.max_concurrency();
    let default_wait = config.element_query_wait.copied();
    let pooled_tests = prepared
        .iter()
        .filter(|test| matches!(test, QueuedTest::Ready(test) if test.element_query_wait == default_wait))
        .count();
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
            config.spare_sessions.unwrap_or(max_concurrency),
            pooled_tests,
            default_wait,
            requests_tx,
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

/// A test whose metadata (name, session settings) was read successfully.
struct PreparedTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    index: usize,
    name: String,
    group: Option<String>,
    timeouts: Option<Timeouts>,
    element_query_wait: Option<ElementQueryWait>,
    test: Box<dyn BrowserTest<Context, TestError>>,
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
    config: &ExecutionConfig<'_>,
) -> Group
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let (parallelism, name, run_always, entries) = tests.into_parts();
    let named = name.is_some();
    let path = match (parent_path, name) {
        (Some(parent), Some(name)) => Some(format!("{parent} / {name}")),
        (None, Some(name)) => Some(name),
        (parent, None) => parent.map(str::to_owned),
    };
    let id = *next_group;
    *next_group += 1;
    let children = entries
        .into_iter()
        .map(|entry| match entry {
            BrowserTestEntry::Test(test) => {
                let index = prepared.len();
                prepared.push(prepare_test(index, test, path.clone(), config));
                Node::Test(index)
            }
            BrowserTestEntry::Group(group) => Node::Group(prepare_group(
                group,
                path.as_deref(),
                prepared,
                next_group,
                config,
            )),
        })
        .collect();
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
    config: &ExecutionConfig<'_>,
) -> QueuedTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    // Used in the panic report. We need a fallback for the case that `.name()` panics.
    let mut name = format!("unnamed test at index {index}");
    let metadata = std::panic::catch_unwind(AssertUnwindSafe(|| {
        name = test.name().into_owned();
        (
            resolve_webdriver_timeouts(test.as_ref(), config.timeouts),
            resolve_element_query_wait(test.as_ref(), config.element_query_wait),
        )
    }));
    match metadata {
        Ok((timeouts, element_query_wait)) => QueuedTest::Ready(PreparedTest {
            index,
            name,
            group,
            timeouts,
            element_query_wait,
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
    fn keep_starting(&self) -> bool {
        self.keep_starting.load(Ordering::SeqCst)
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
            TestOutcome::Passed => tracing::info!(
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
        // Tests not started because of fail-fast are not recorded.
        Node::Test(index) if run_always || env.keep_starting() => run_test(env, *index).await,
        Node::Test(_) => {}
        Node::Group(group) => run_group(env, group, run_always).await,
    }
}

/// Run the test at `index` in a fresh session and return once its body finished. Its session
/// quits, and its record is completed, in the background.
async fn run_test<Context, TestError>(env: &Env<'_, Context, TestError>, index: usize)
where
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
            let record = BrowserTestRecord {
                index: *index,
                name: name.clone(),
                outcome: TestOutcome::Panicked,
                group: group.clone(),
                session: None,
                body: None,
                teardown: None,
                steps: BTreeMap::new(),
            };
            env.finish(record, Err(report));
            return;
        }
    };

    let requested = Instant::now();
    let ticket = if test.element_query_wait == env.pool.default_wait {
        env.pool.take(env.keep_starting()).await
    } else {
        env.pool.dedicated(test.element_query_wait).await
    };
    let wait = requested.elapsed();
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
                    creation: ticket.creation,
                },
            }
        }
        Err(failure) => failure,
    };

    let record = BrowserTestRecord {
        index,
        name: test.name.clone(),
        outcome: TestOutcome::Failed,
        group: test.group.clone(),
        session: Some(SessionTiming {
            creation: failure.creation,
            wait,
        }),
        body: None,
        teardown: None,
        steps: BTreeMap::new(),
    };
    let report = failure.report.context(BrowserTestError::RunTest {
        test_name: test.name.clone(),
    });
    env.finish(record, Err(report));
}

/// A test assigned to a session.
struct Job {
    test: usize,
    /// How long the test waited for its session.
    wait: Duration,
    /// Completed once the test body finished, before the session quits.
    body_done: oneshot::Sender<()>,
}

/// A session ready to run a test.
struct Ticket {
    job: oneshot::Sender<Job>,
    /// How long creating the session took.
    creation: Duration,
}

/// A session that could not be created.
struct CreationFailure {
    report: Report,
    creation: Duration,
}

type TicketResult = Result<Ticket, CreationFailure>;

/// Where a new session is offered.
enum Delivery {
    Pool,
    Dedicated(oneshot::Sender<TicketResult>),
}

/// A request to create a session.
struct WorkerRequest {
    session: usize,
    element_query_wait: Option<ElementQueryWait>,
    delivery: Delivery,
}

/// Keeps fresh sessions with the runner's default settings ready for starting tests.
struct SessionPool {
    state: Mutex<PoolState>,
    available: Notify,
    /// Sessions to keep ready beyond those requested.
    spare: usize,
    default_wait: Option<ElementQueryWait>,
    requests: mpsc::UnboundedSender<Option<WorkerRequest>>,
}

struct PoolState {
    /// Created sessions not yet taken by a test.
    idle: VecDeque<TicketResult>,
    /// Sessions being created for the pool.
    creating: usize,
    /// Tests waiting for a pool session.
    waiting: usize,
    /// Pool tests that have not requested a session yet.
    upcoming: usize,
    next_session: usize,
    closed: bool,
}

impl SessionPool {
    fn new(
        spare: usize,
        upcoming: usize,
        default_wait: Option<ElementQueryWait>,
        requests: mpsc::UnboundedSender<Option<WorkerRequest>>,
    ) -> Self {
        Self {
            state: Mutex::new(PoolState {
                idle: VecDeque::new(),
                creating: 0,
                waiting: 0,
                upcoming,
                next_session: 0,
                closed: false,
            }),
            available: Notify::new(),
            spare,
            default_wait,
            requests,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Create sessions for waiting tests and, unless tests stopped starting, spares for upcoming
    /// ones.
    fn replenish(&self, keep_starting: bool) {
        self.replenish_locked(&mut self.lock(), keep_starting);
    }

    fn replenish_locked(&self, state: &mut PoolState, keep_starting: bool) {
        let spares = if keep_starting {
            self.spare.min(state.upcoming)
        } else {
            0
        };
        while !state.closed && state.idle.len() + state.creating < state.waiting + spares {
            state.creating += 1;
            let session = state.next_session;
            state.next_session += 1;
            let _ = self.requests.send(Some(WorkerRequest {
                session,
                element_query_wait: self.default_wait,
                delivery: Delivery::Pool,
            }));
        }
    }

    /// Take a session for a starting test, waiting until one is ready.
    async fn take(&self, keep_starting: bool) -> TicketResult {
        {
            let mut state = self.lock();
            state.upcoming = state.upcoming.saturating_sub(1);
            state.waiting += 1;
            self.replenish_locked(&mut state, keep_starting);
        }
        loop {
            let mut available = pin!(self.available.notified());
            available.as_mut().enable();
            {
                let mut state = self.lock();
                if let Some(ticket) = state.idle.pop_front() {
                    state.waiting -= 1;
                    self.replenish_locked(&mut state, keep_starting);
                    return ticket;
                }
            }
            available.await;
        }
    }

    /// Create a session with non-default settings for one test.
    async fn dedicated(&self, element_query_wait: Option<ElementQueryWait>) -> TicketResult {
        let creation_start = Instant::now();
        let (ticket_tx, ticket_rx) = oneshot::channel();
        let session = {
            let mut state = self.lock();
            let session = state.next_session;
            state.next_session += 1;
            session
        };
        let _ = self.requests.send(Some(WorkerRequest {
            session,
            element_query_wait,
            delivery: Delivery::Dedicated(ticket_tx),
        }));
        ticket_rx.await.unwrap_or_else(|_| {
            Err(CreationFailure {
                report: rootcause::report!("the browser session could not be created"),
                creation: creation_start.elapsed(),
            })
        })
    }

    fn deliver(&self, delivery: Delivery, ticket: TicketResult) {
        match delivery {
            Delivery::Pool => {
                {
                    let mut state = self.lock();
                    state.creating -= 1;
                    if !state.closed {
                        state.idle.push_back(ticket);
                    }
                }
                self.available.notify_waiters();
            }
            Delivery::Dedicated(ticket_tx) => {
                let _ = ticket_tx.send(ticket);
            }
        }
    }

    /// Create no further sessions and let idle ones quit.
    fn close(&self) {
        {
            let mut state = self.lock();
            state.closed = true;
            state.idle.clear();
        }
        let _ = self.requests.send(None);
    }
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
    creation: Duration,
    wait: Duration,
    body: Option<Duration>,
    steps: BTreeMap<String, StepStats>,
    /// Set if the test body panicked.
    panic_message: Option<String>,
    quit_start: Option<Instant>,
}

/// Create a session, offer it, run the test it is assigned, and quit it.
async fn session_worker<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    request: WorkerRequest,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let config = env.config;
    let activity = Activity::Session(request.session);
    env.board
        .enter(activity, "a browser session", Phase::CreatingSession);
    let creation_start = Instant::now();
    let mut delivery = Some(request.delivery);
    let mut job_run = None;
    let session_result = config
        .chrome
        .session()
        .with_caps(|caps: &mut ChromeCapabilities| {
            configure_chrome_capabilities(caps, config.visible, config.chrome_capabilities_setups)
        })
        .with_config(|builder| match request.element_query_wait {
            Some(wait) => builder.poller(Arc::new(wait.into_thirtyfour_poller())),
            None => builder,
        })
        .run(async |session: &Session| {
            let creation = creation_start.elapsed();
            env.board.clear(activity);
            env.warn_if_slow_session("Creating a browser session", creation);
            let (job_tx, job_rx) = oneshot::channel();
            if let Some(delivery) = delivery.take() {
                env.pool.deliver(
                    delivery,
                    Ok(Ticket {
                        job: job_tx,
                        creation,
                    }),
                );
            }
            // An error means no test needs this session anymore.
            let Ok(job) = job_rx.await else {
                return Ok(());
            };
            run_job(env, session, job, creation, &mut job_run).await
        })
        .await;
    env.board.clear(activity);

    if let Some(delivery) = delivery {
        // The session could not be created, so it was never offered.
        let report = match session_result {
            Err(error) => error.into_dynamic(),
            Ok(()) => rootcause::report!("the browser session ended before it could be used"),
        };
        env.pool.deliver(
            delivery,
            Err(CreationFailure {
                report,
                creation: creation_start.elapsed(),
            }),
        );
        return;
    }
    let Some(job_run) = job_run else {
        return;
    };

    let QueuedTest::Ready(test) = &env.tests[job_run.index] else {
        unreachable!("only ready tests are assigned to sessions");
    };
    let teardown = job_run.quit_start.map(|quit_start| quit_start.elapsed());
    env.board.clear(Activity::Test(job_run.index));
    if let Some(teardown) = teardown {
        env.warn_if_slow_session(
            &format!("Quitting the session of browser test '{}'", test.name),
            teardown,
        );
    }
    let (outcome, result) = test_outcome(&test.name, job_run.panic_message, session_result);
    let record = BrowserTestRecord {
        index: job_run.index,
        name: test.name.clone(),
        outcome,
        group: test.group.clone(),
        session: Some(SessionTiming {
            creation: job_run.creation,
            wait: job_run.wait,
        }),
        body: job_run.body,
        teardown,
        steps: job_run.steps,
    };
    env.finish(record, result);
}

/// Run the assigned test in `session`, then report that its body finished.
async fn run_job<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    session: &Session,
    job: Job,
    creation: Duration,
    job_run: &mut Option<JobRun>,
) -> Result<(), Report>
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let QueuedTest::Ready(test) = &env.tests[job.test] else {
        unreachable!("only ready tests are assigned to sessions");
    };
    let run = job_run.insert(JobRun {
        index: job.test,
        creation,
        wait: job.wait,
        body: None,
        steps: BTreeMap::new(),
        panic_message: None,
        quit_start: None,
    });
    let result = match test.timeouts {
        Some(timeouts) => session
            .update_timeouts(timeouts.into_thirtyfour_timeout_configuration())
            .await
            .map_err(Report::<rootcause::markers::Dynamic>::from),
        None => Ok(()),
    };
    let result = match result {
        Ok(()) => run_body(env, session, test, run).await,
        Err(error) => {
            env.stop_starting_on_fail_fast();
            Err(error)
        }
    };
    // Let the next test start while this session quits.
    let _ = job.body_done.send(());
    env.board.enter(
        Activity::Test(test.index),
        format!("browser test '{}'", test.name),
        Phase::QuittingSession,
    );
    run.quit_start = Some(Instant::now());
    result
}

/// The outcome of a test whose session ended with `session_result`.
fn test_outcome(
    name: &str,
    panic_message: Option<String>,
    session_result: Result<(), Report<chrome_for_testing_manager::ChromeForTestingError>>,
) -> (TestOutcome, Result<(), Report<BrowserTestError>>) {
    if let Some(message) = panic_message {
        let report = Report::new(BrowserTestError::Panic {
            test_name: name.to_owned(),
            message,
        });
        return (TestOutcome::Panicked, Err(report));
    }
    match session_result {
        Ok(()) => (TestOutcome::Passed, Ok(())),
        Err(err) => (
            TestOutcome::Failed,
            Err(err.context(BrowserTestError::RunTest {
                test_name: name.to_owned(),
            })),
        ),
    }
}

/// Run the body of `test`, recording its duration and steps into `run`.
///
/// Returns an error if the test failed or panicked (then `run.panic_message` is set).
async fn run_body<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    session: &Session,
    test: &PreparedTest<Context, TestError>,
    run: &mut JobRun,
) -> Result<(), Report>
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let name = test.name.as_str();
    tracing::info!("Executing browser test: {name}");
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
    let result = recorder
        .scope(AssertUnwindSafe(test.test.run(session, env.context)).catch_unwind())
        .instrument(span)
        .await;
    run.body = Some(start.elapsed());
    run.steps = recorder.take();
    if !matches!(result, Ok(Ok(()))) {
        // Don't let further tests start while this session quits.
        env.stop_starting_on_fail_fast();
    }

    match result {
        Ok(result) => result.map_err(Report::into_dynamic),
        Err(payload) => {
            let message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{name}' panicked: {message}");
            run.panic_message = Some(message);
            // Only ends the session. The runner reports the panic itself.
            Err(rootcause::report!("browser test '{name}' panicked"))
        }
    }
}

fn resolve_webdriver_timeouts<Context, TestError>(
    test: &dyn BrowserTest<Context, TestError>,
    runner_timeouts: Option<&Timeouts>,
) -> Option<Timeouts>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    test.timeouts().or_else(|| runner_timeouts.copied())
}

fn resolve_element_query_wait<Context, TestError>(
    test: &dyn BrowserTest<Context, TestError>,
    runner_wait: Option<&ElementQueryWait>,
) -> Option<ElementQueryWait>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    test.element_query_wait().or_else(|| runner_wait.copied())
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
        setup(caps)?;
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
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use assertr::prelude::*;
    use rootcause::Report;
    use thirtyfour::{BrowserCapabilitiesHelper, WebDriver};

    use super::*;

    mod resolve_webdriver_timeouts {
        use super::*;

        struct TimeoutOverrideTest {
            timeouts: Option<Timeouts>,
        }

        #[async_trait::async_trait]
        impl BrowserTest for TimeoutOverrideTest {
            fn name(&self) -> Cow<'_, str> {
                Cow::Borrowed("timeout override")
            }

            fn timeouts(&self) -> Option<Timeouts> {
                self.timeouts
            }

            async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
                Ok(())
            }
        }

        #[test]
        fn uses_test_override_before_runner_default() {
            let runner_timeouts = Timeouts::builder()
                .script_timeout(Duration::from_secs(10))
                .page_load_timeout(Duration::from_secs(10))
                .implicit_wait_timeout(Duration::from_secs(10))
                .build();
            let test_timeouts = Timeouts::builder()
                .script_timeout(Duration::from_secs(5))
                .page_load_timeout(Duration::from_secs(5))
                .implicit_wait_timeout(Duration::from_secs(5))
                .build();
            let test = TimeoutOverrideTest {
                timeouts: Some(test_timeouts),
            };

            let resolved = resolve_webdriver_timeouts(&test, Some(&runner_timeouts));

            assert_that!(resolved).is_equal_to(Some(test_timeouts));
        }

        #[test]
        fn falls_back_to_runner_default() {
            let runner_timeouts = Timeouts::builder()
                .script_timeout(Duration::from_secs(10))
                .page_load_timeout(Duration::from_secs(10))
                .implicit_wait_timeout(Duration::from_secs(10))
                .build();
            let test = TimeoutOverrideTest { timeouts: None };

            let resolved = resolve_webdriver_timeouts(&test, Some(&runner_timeouts));

            assert_that!(resolved).is_equal_to(Some(runner_timeouts));
        }

        #[test]
        fn preserves_unconfigured_default() {
            let test = TimeoutOverrideTest { timeouts: None };

            let resolved = resolve_webdriver_timeouts(&test, None);

            assert_that!(resolved).is_none();
        }
    }

    mod resolve_element_query_wait {
        use super::*;

        struct ElementQueryWaitOverrideTest {
            wait: Option<ElementQueryWait>,
        }

        #[async_trait::async_trait]
        impl BrowserTest for ElementQueryWaitOverrideTest {
            fn name(&self) -> Cow<'_, str> {
                Cow::Borrowed("element query wait override")
            }

            fn element_query_wait(&self) -> Option<ElementQueryWait> {
                self.wait
            }

            async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
                Ok(())
            }
        }

        #[test]
        fn uses_test_override_before_runner_default() {
            let runner_wait =
                ElementQueryWait::new(Duration::from_secs(10), Duration::from_secs(1))
                    .expect("non-zero interval is valid");
            let test_wait =
                ElementQueryWait::new(Duration::from_secs(5), Duration::from_millis(500))
                    .expect("non-zero interval is valid");
            let test = ElementQueryWaitOverrideTest {
                wait: Some(test_wait),
            };

            let resolved = resolve_element_query_wait(&test, Some(&runner_wait));

            assert_that!(resolved).is_equal_to(Some(test_wait));
        }

        #[test]
        fn falls_back_to_runner_default() {
            let runner_wait =
                ElementQueryWait::new(Duration::from_secs(10), Duration::from_millis(500))
                    .expect("non-zero interval is valid");
            let test = ElementQueryWaitOverrideTest { wait: None };

            let resolved = resolve_element_query_wait(&test, Some(&runner_wait));

            assert_that!(resolved).is_equal_to(Some(runner_wait));
        }

        #[test]
        fn preserves_unconfigured_default() {
            let test = ElementQueryWaitOverrideTest { wait: None };

            let resolved = resolve_element_query_wait(&test, None);

            assert_that!(resolved).is_none();
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

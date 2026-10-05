use std::{
    any::Any,
    collections::{BTreeMap, VecDeque},
    future::Future,
    num::NonZeroUsize,
    panic::AssertUnwindSafe,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrome_for_testing_manager::{Chromedriver, Session};
use futures_util::FutureExt as _;
use rootcause::Report;
use thirtyfour::{ChromeCapabilities, ChromiumLikeCapabilities, error::WebDriverResult};
use tracing::Instrument as _;

use crate::progress::{Phase, ProgressBoard};
use crate::report::{
    BrowserTestRecord, FormatDuration, SessionAcquisition, StepStats, TestOutcome, timing_breakdown,
};
use crate::scheduler::BrowserTestFailures;
use crate::session::{SessionBaseline, SessionSettings};
use crate::step::StepRecorder;
use crate::{
    BrowserTest, BrowserTestError, BrowserTestFailurePolicy, BrowserTests, BrowserTimeouts,
    ElementQueryWaitConfig, ProgressWarnings, SessionRequirement, SessionReset,
};

pub(crate) type ChromeCapabilitiesSetup =
    dyn Fn(&mut ChromeCapabilities) -> WebDriverResult<()> + Send + Sync + 'static;

/// Everything the runner configured for executing tests.
pub(crate) struct ExecutionConfig<'a> {
    pub(crate) chromedriver: &'a Chromedriver,
    pub(crate) visible: bool,
    pub(crate) webdriver_timeouts: Option<&'a BrowserTimeouts>,
    pub(crate) element_query_wait: Option<&'a ElementQueryWaitConfig>,
    pub(crate) chrome_capabilities_setups: &'a [Arc<ChromeCapabilitiesSetup>],
    pub(crate) session_reuse: bool,
    pub(crate) session_resets: &'a [Arc<dyn SessionReset>],
    pub(crate) failure_policy: BrowserTestFailurePolicy,
    pub(crate) progress_warnings: ProgressWarnings,
    pub(crate) max_parallel_tests: NonZeroUsize,
}

/// Run `tests`, each parallel slot reusing its session for consecutive compatible tests.
///
/// Returns a record per started test (in test order) and the run's result.
pub(crate) async fn execute_tests<Context, TestError>(
    config: &ExecutionConfig<'_>,
    context: &Context,
    tests: BrowserTests<Context, TestError>,
) -> (Vec<BrowserTestRecord>, Result<(), Report<BrowserTestError>>)
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let queue = tests
        .into_vec()
        .into_iter()
        .enumerate()
        .map(|(index, test)| prepare_test(index, test, config))
        .collect();
    let slots = config.max_parallel_tests.get();
    let shared = Shared {
        queue: Mutex::new(queue),
        keep_starting: AtomicBool::new(true),
        failure_policy: config.failure_policy,
        failures: Mutex::new(BrowserTestFailures::default()),
        records: Mutex::new(Vec::new()),
        board: ProgressBoard::new(slots),
    };

    let workers = futures_util::future::join_all((0..slots).map(|slot| {
        Box::pin(run_slot(slot, &shared, config, context))
            as Pin<Box<dyn Future<Output = ()> + Send + '_>>
    }));
    // The watchdog never finishes on its own; it is dropped once all workers are done.
    let watchdog = shared.board.watch(config.progress_warnings);
    futures_util::future::select(pin!(workers), pin!(watchdog)).await;

    let Shared {
        failures, records, ..
    } = shared;
    let mut records = records.into_inner().unwrap_or_else(PoisonError::into_inner);
    records.sort_by_key(|record| record.index);
    let failures = failures
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    let result = if slots == 1 && config.failure_policy == BrowserTestFailurePolicy::FailFast {
        failures.into_first_result()
    } else {
        failures.into_result()
    };
    (records, result)
}

/// A test whose metadata (name, session settings) was read successfully.
struct PreparedTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    index: usize,
    name: String,
    settings: SessionSettings,
    requirement: SessionRequirement,
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
        report: Report<BrowserTestError>,
    },
}

fn prepare_test<Context, TestError>(
    index: usize,
    test: Box<dyn BrowserTest<Context, TestError>>,
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
        let settings = SessionSettings {
            timeouts: resolve_webdriver_timeouts(test.as_ref(), config.webdriver_timeouts),
            element_query_wait: resolve_element_query_wait(
                test.as_ref(),
                config.element_query_wait,
            ),
        };
        (settings, test.session())
    }));
    match metadata {
        Ok((settings, requirement)) => QueuedTest::Ready(PreparedTest {
            index,
            name,
            settings,
            requirement,
            test,
        }),
        Err(payload) => {
            let message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{name}' panicked: {message}");
            QueuedTest::Broken {
                index,
                report: Report::new(BrowserTestError::Panic {
                    test_name: name.clone(),
                    message,
                }),
                name,
            }
        }
    }
}

/// State shared by all parallel slots.
struct Shared<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    queue: Mutex<VecDeque<QueuedTest<Context, TestError>>>,
    keep_starting: AtomicBool,
    failure_policy: BrowserTestFailurePolicy,
    failures: Mutex<BrowserTestFailures>,
    records: Mutex<Vec<BrowserTestRecord>>,
    board: ProgressBoard,
}

impl<Context, TestError> Shared<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    /// The next test to start, in a session of its own.
    fn next_test(&self) -> Option<PreparedTest<Context, TestError>> {
        self.pop_front_if(|_| true)
    }

    /// The next test, if it may reuse a session created with `settings`.
    fn next_test_for_session(
        &self,
        settings: SessionSettings,
    ) -> Option<PreparedTest<Context, TestError>> {
        self.pop_front_if(|test| {
            test.requirement == SessionRequirement::Shared && test.settings == settings
        })
    }

    /// Pop the front test if it matches `accept`. Broken tests at the front fail on the way.
    fn pop_front_if(
        &self,
        accept: impl Fn(&PreparedTest<Context, TestError>) -> bool,
    ) -> Option<PreparedTest<Context, TestError>> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if !self.keep_starting.load(Ordering::SeqCst) {
                return None;
            }
            match queue.front()? {
                QueuedTest::Broken { .. } => {
                    let Some(QueuedTest::Broken {
                        index,
                        name,
                        report,
                    }) = queue.pop_front()
                    else {
                        unreachable!("the front of the queue was just checked");
                    };
                    let record = BrowserTestRecord {
                        index,
                        name,
                        outcome: TestOutcome::Panicked,
                        slot: 0,
                        session: SessionAcquisition::None,
                        body: None,
                        teardown: None,
                        steps: BTreeMap::new(),
                    };
                    self.finish(record, Err(report));
                }
                QueuedTest::Ready(test) if accept(test) => {
                    let Some(QueuedTest::Ready(test)) = queue.pop_front() else {
                        unreachable!("the front of the queue was just checked");
                    };
                    return Some(test);
                }
                QueuedTest::Ready(_) => return None,
            }
        }
    }

    /// Put a test back to the front of the queue (its session could not be reset).
    fn requeue(&self, test: PreparedTest<Context, TestError>) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_front(QueuedTest::Ready(test));
    }

    /// With [`BrowserTestFailurePolicy::FailFast`], start no further tests (after a failure).
    fn stop_starting_on_fail_fast(&self) {
        if self.failure_policy == BrowserTestFailurePolicy::FailFast {
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
}

/// One parallel slot: runs tests until the queue is empty, starting a new session whenever the
/// next test cannot reuse the current one.
async fn run_slot<Context, TestError>(
    slot: usize,
    shared: &Shared<Context, TestError>,
    config: &ExecutionConfig<'_>,
    context: &Context,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    while let Some(test) = shared.next_test() {
        run_session(slot, test, shared, config, context).await;
    }
    shared.board.clear(slot);
}

/// The test currently using a session, and its timings so far.
struct CurrentTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    test: PreparedTest<Context, TestError>,
    session: SessionAcquisition,
    body: Option<Duration>,
    steps: BTreeMap<String, StepStats>,
    /// Set if the test body panicked.
    panic_message: Option<String>,
}

impl<Context, TestError> CurrentTest<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    fn new(test: PreparedTest<Context, TestError>, session: SessionAcquisition) -> Self {
        Self {
            test,
            session,
            body: None,
            steps: BTreeMap::new(),
            panic_message: None,
        }
    }

    fn into_record(
        self,
        slot: usize,
        outcome: TestOutcome,
        teardown: Option<Duration>,
    ) -> BrowserTestRecord {
        BrowserTestRecord {
            index: self.test.index,
            name: self.test.name,
            outcome,
            slot,
            session: self.session,
            body: self.body,
            teardown,
            steps: self.steps,
        }
    }
}

/// Create a session for `first` and keep running compatible tests in it.
///
/// The session ends after a test that failed, panicked, or must not share its session, when the
/// next test needs other session settings, or when the session could not be reset.
async fn run_session<Context, TestError>(
    slot: usize,
    first: PreparedTest<Context, TestError>,
    shared: &Shared<Context, TestError>,
    config: &ExecutionConfig<'_>,
    context: &Context,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let settings = first.settings;
    let reusable = config.session_reuse && first.requirement == SessionRequirement::Shared;
    shared
        .board
        .enter(slot, &first.name, Phase::CreatingSession);
    let creation_start = Instant::now();
    let mut current = CurrentTest::new(
        first,
        SessionAcquisition::Created {
            duration: Duration::ZERO,
        },
    );
    let mut quit_start = None;

    let session_result = config
        .chromedriver
        .session()
        .with_caps(|caps: &mut ChromeCapabilities| {
            configure_chrome_capabilities(caps, config.visible, config.chrome_capabilities_setups)
        })
        .with_config(|builder| match settings.element_query_wait {
            Some(wait) => builder.poller(Arc::new(wait.into_thirtyfour_poller())),
            None => builder,
        })
        .run(async |session: &Session| {
            let result = drive_session(
                slot,
                session,
                &mut current,
                creation_start,
                reusable,
                shared,
                config,
                context,
            )
            .await;
            shared
                .board
                .enter(slot, &current.test.name, Phase::QuittingSession);
            quit_start = Some(Instant::now());
            result
        })
        .await;

    let teardown = quit_start.map(|quit_start| quit_start.elapsed());
    if quit_start.is_none() {
        // The session could not be created.
        current.session = SessionAcquisition::Created {
            duration: creation_start.elapsed(),
        };
    }
    warn_if_slow_session(
        config,
        "Quitting the session of",
        &current.test.name,
        teardown,
    );

    let (outcome, result) = match current.panic_message.take() {
        Some(message) => (
            TestOutcome::Panicked,
            Err(Report::new(BrowserTestError::Panic {
                test_name: current.test.name.clone(),
                message,
            })),
        ),
        None => match session_result {
            Ok(()) => (TestOutcome::Passed, Ok(())),
            Err(err) => (
                TestOutcome::Failed,
                Err(err.context(BrowserTestError::RunTest {
                    test_name: current.test.name.clone(),
                })),
            ),
        },
    };
    shared.finish(current.into_record(slot, outcome, teardown), result);
}

/// Run `current` and then every compatible next test in `session`.
///
/// Returns the result of the last test run. Earlier tests passed and are recorded already.
#[allow(clippy::too_many_arguments)]
async fn drive_session<Context, TestError>(
    slot: usize,
    session: &Session,
    current: &mut CurrentTest<Context, TestError>,
    creation_start: Instant,
    reusable: bool,
    shared: &Shared<Context, TestError>,
    config: &ExecutionConfig<'_>,
    context: &Context,
) -> Result<(), Report>
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let settings = current.test.settings;
    let setup = async {
        if let Some(timeouts) = settings.timeouts {
            session
                .update_timeouts(timeouts.into_thirtyfour_timeout_configuration())
                .await?;
        }
        if !reusable {
            return Ok::<_, Report>(None);
        }
        match SessionBaseline::capture(session).await {
            Ok(baseline) => Ok(Some(baseline)),
            Err(err) => {
                tracing::warn!(
                    "Not reusing the session of browser test '{}': failed to capture its initial state: {err}",
                    current.test.name,
                );
                Ok(None)
            }
        }
    }
    .await;
    let creation = creation_start.elapsed();
    current.session = SessionAcquisition::Created { duration: creation };
    warn_if_slow_session(
        config,
        "Creating a session for",
        &current.test.name,
        Some(creation),
    );
    let mut baseline = setup?;

    loop {
        run_body(slot, session, current, shared, config, context).await?;

        let Some(baseline) = baseline.as_mut() else {
            return Ok(());
        };
        let Some(next) = shared.next_test_for_session(settings) else {
            return Ok(());
        };

        shared
            .board
            .enter(slot, &next.name, Phase::ResettingSession);
        let reset_start = Instant::now();
        let reset_result = baseline.reset(session, config.session_resets).await;
        let reset = reset_start.elapsed();
        warn_if_slow_session(config, "Resetting the session for", &next.name, Some(reset));
        if let Err(err) = reset_result {
            tracing::warn!(
                "Discarding the session of browser test '{}': failed to reset it for '{}': {err}",
                current.test.name,
                next.name,
            );
            shared.requeue(next);
            return Ok(());
        }

        let finished = std::mem::replace(
            current,
            CurrentTest::new(next, SessionAcquisition::Reused { reset }),
        );
        shared.finish(
            finished.into_record(slot, TestOutcome::Passed, None),
            Ok(()),
        );
    }
}

/// Run the body of `current`, recording its duration and steps.
///
/// Returns an error if the test failed or panicked (then `current.panic_message` is set).
async fn run_body<Context, TestError>(
    slot: usize,
    session: &Session,
    current: &mut CurrentTest<Context, TestError>,
    shared: &Shared<Context, TestError>,
    config: &ExecutionConfig<'_>,
    context: &Context,
) -> Result<(), Report>
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let name = current.test.name.as_str();
    tracing::info!("Executing browser test: {name}");
    shared.board.enter(slot, name, Phase::RunningTest);
    let recorder = StepRecorder::new(config.progress_warnings.slow_step());
    let span = tracing::info_span!("browser_test", test = %name, slot);
    let start = Instant::now();
    let result = recorder
        .scope(AssertUnwindSafe(current.test.test.run(session, context)).catch_unwind())
        .instrument(span)
        .await;
    current.body = Some(start.elapsed());
    current.steps = recorder.take();
    if !matches!(result, Ok(Ok(()))) {
        // Don't let other slots start tests while this session quits.
        shared.stop_starting_on_fail_fast();
    }

    match result {
        Ok(result) => result.map_err(Report::into_dynamic),
        Err(payload) => {
            let message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{name}' panicked: {message}");
            current.panic_message = Some(message);
            // Only ends the session; the runner reports the panic itself.
            Err(rootcause::report!("browser test '{name}' panicked"))
        }
    }
}

fn warn_if_slow_session(
    config: &ExecutionConfig<'_>,
    what: &str,
    test_name: &str,
    duration: Option<Duration>,
) {
    if let (Some(duration), Some(threshold)) = (duration, config.progress_warnings.session())
        && duration > threshold
    {
        tracing::warn!(
            test = %test_name,
            duration_ms = duration.as_millis(),
            "{what} browser test '{test_name}' took {}.",
            FormatDuration(duration),
        );
    }
}

fn resolve_webdriver_timeouts<Context, TestError>(
    test: &dyn BrowserTest<Context, TestError>,
    runner_timeouts: Option<&BrowserTimeouts>,
) -> Option<BrowserTimeouts>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    test.timeouts().or_else(|| runner_timeouts.copied())
}

fn resolve_element_query_wait<Context, TestError>(
    test: &dyn BrowserTest<Context, TestError>,
    runner_wait: Option<&ElementQueryWaitConfig>,
) -> Option<ElementQueryWaitConfig>
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
            timeouts: Option<BrowserTimeouts>,
        }

        #[async_trait::async_trait]
        impl BrowserTest for TimeoutOverrideTest {
            fn name(&self) -> Cow<'_, str> {
                Cow::Borrowed("timeout override")
            }

            fn timeouts(&self) -> Option<BrowserTimeouts> {
                self.timeouts
            }

            async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
                Ok(())
            }
        }

        #[test]
        fn uses_test_override_before_runner_default() {
            let runner_timeouts = BrowserTimeouts::builder()
                .script_timeout(Duration::from_secs(10))
                .page_load_timeout(Duration::from_secs(10))
                .implicit_wait_timeout(Duration::from_secs(10))
                .build();
            let test_timeouts = BrowserTimeouts::builder()
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
            let runner_timeouts = BrowserTimeouts::builder()
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
            wait: Option<ElementQueryWaitConfig>,
        }

        #[async_trait::async_trait]
        impl BrowserTest for ElementQueryWaitOverrideTest {
            fn name(&self) -> Cow<'_, str> {
                Cow::Borrowed("element query wait override")
            }

            fn element_query_wait(&self) -> Option<ElementQueryWaitConfig> {
                self.wait
            }

            async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
                Ok(())
            }
        }

        #[test]
        fn uses_test_override_before_runner_default() {
            let runner_wait = ElementQueryWaitConfig::builder()
                .timeout(Duration::from_secs(10))
                .interval(Duration::from_secs(1))
                .build();
            let test_wait = ElementQueryWaitConfig::builder()
                .timeout(Duration::from_secs(5))
                .interval(Duration::from_millis(500))
                .build();
            let test = ElementQueryWaitOverrideTest {
                wait: Some(test_wait),
            };

            let resolved = resolve_element_query_wait(&test, Some(&runner_wait));

            assert_that!(resolved).is_equal_to(Some(test_wait));
        }

        #[test]
        fn falls_back_to_runner_default() {
            let runner_wait = ElementQueryWaitConfig::builder()
                .timeout(Duration::from_secs(10))
                .interval(Duration::from_millis(500))
                .build();
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

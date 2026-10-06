use std::{fmt, path::PathBuf, sync::Arc, time::Instant};

use chrome_for_testing_manager::{
    Channel, ChromeBinary, ChromeForTesting, ChromeForTestingConfig, VersionRequest,
};
use rootcause::Report;
use rootcause::prelude::ResultExt;
use thirtyfour::{ChromeCapabilities, error::WebDriverResult};

use crate::driver_output::{DriverOutputCapture, attach_browser_driver_output_to_result};
use crate::env::{InvalidEnvVar, env_flag};
use crate::execution::{ChromeCapabilitiesSetup, Execution, ExecutionConfig, execute_tests};
use crate::pause::{self, PauseDecision};
use crate::report::BrowserTestRunReport;
use crate::report_consumer::RunReportConsumer;
use crate::{
    BrowserTestError, BrowserTests, DriverOutput, ElementQueryWait, FailurePolicy, Pause,
    ProgressWarnings, Timeouts,
};

pub(crate) const DEFAULT_VISIBLE_ENV: &str = "BROWSER_TEST_VISIBLE";

/// Whether the browser runs headless or visibly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Visibility {
    /// Run the browser headless.
    #[default]
    Headless,

    /// Run the browser visibly, e.g. to watch or debug tests.
    Visible,
}

impl Visibility {
    /// Read the visibility from `BROWSER_TEST_VISIBLE`.
    ///
    /// See [`Self::from_env_var`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a boolean flag.
    pub fn from_env() -> Result<Option<Self>, InvalidEnvVar> {
        Self::from_env_var(DEFAULT_VISIBLE_ENV)
    }

    /// Read the visibility from the boolean flag `env_var`: enabled means [`Self::Visible`].
    ///
    /// Returns `None` if the variable is unset or empty, so the caller picks the default:
    /// `Visibility::from_env()?.unwrap_or_default()`. `1`, `true`, `yes`, `on`, and `enabled`
    /// enable the flag; `0`, `false`, `no`, `off`, and `disabled` disable it (ignoring case). The
    /// variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a boolean flag.
    pub fn from_env_var(env_var: impl AsRef<str>) -> Result<Option<Self>, InvalidEnvVar> {
        Ok(env_flag(env_var.as_ref())?.map(|visible| {
            if visible {
                Self::Visible
            } else {
                Self::Headless
            }
        }))
    }

    pub(crate) const fn is_visible(self) -> bool {
        matches!(self, Self::Visible)
    }
}

/// Runs [`crate::BrowserTest`] implementations through Chrome for Testing.
#[derive(Clone)]
pub struct BrowserTestRunner {
    channel: Channel,
    visibility: Visibility,
    pause: Pause,
    failure_policy: FailurePolicy,
    timeouts: Option<Timeouts>,
    element_query_wait: Option<ElementQueryWait>,
    chrome_capabilities_setups: Vec<Arc<ChromeCapabilitiesSetup>>,
    driver_output: DriverOutput,
    chrome_for_testing_cache_dir: Option<PathBuf>,
    headless_chrome_binary: ChromeBinary,
    spare_sessions: Option<usize>,
    progress_warnings: ProgressWarnings,
    report_consumers: Vec<Arc<dyn RunReportConsumer>>,
}

impl Default for BrowserTestRunner {
    fn default() -> Self {
        Self {
            channel: Channel::Stable,
            visibility: Visibility::Headless,
            pause: Pause::disabled(),
            failure_policy: FailurePolicy::FailFast,
            timeouts: None,
            element_query_wait: None,
            chrome_capabilities_setups: Vec::new(),
            driver_output: DriverOutput::disabled(),
            chrome_for_testing_cache_dir: None,
            headless_chrome_binary: ChromeBinary::Chrome,
            spare_sessions: None,
            progress_warnings: ProgressWarnings::default(),
            report_consumers: Vec::new(),
        }
    }
}

impl fmt::Debug for BrowserTestRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserTestRunner")
            .field("channel", &self.channel)
            .field("visibility", &self.visibility)
            .field("pause", &self.pause)
            .field("failure_policy", &self.failure_policy)
            .field("timeouts", &self.timeouts)
            .field("element_query_wait", &self.element_query_wait)
            .field(
                "chrome_capabilities_setup_count",
                &self.chrome_capabilities_setups.len(),
            )
            .field("driver_output", &self.driver_output)
            .field(
                "chrome_for_testing_cache_dir",
                &self.chrome_for_testing_cache_dir,
            )
            .field("headless_chrome_binary", &self.headless_chrome_binary)
            .field("spare_sessions", &self.spare_sessions)
            .field("progress_warnings", &self.progress_warnings)
            .field("report_consumer_count", &self.report_consumers.len())
            .finish()
    }
}

impl BrowserTestRunner {
    /// Create a runner using stable Chrome in headless mode.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Select the Chrome release channel. Defaults to [`Channel::Stable`].
    #[must_use]
    pub fn with_channel(mut self, channel: Channel) -> Self {
        self.channel = channel;
        self
    }

    /// Select whether the browser runs headless or visibly. Defaults to [`Visibility::Headless`].
    #[must_use]
    pub const fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Configure the manual pause before tests run. Defaults to [`Pause::disabled`].
    ///
    /// If the pause is aborted, [`Self::run`] returns successfully without starting the browser or
    /// running any tests. If stdin reaches EOF while waiting for a pause response, [`Self::run`]
    /// returns an error instead.
    #[must_use]
    pub fn with_pause(mut self, pause: Pause) -> Self {
        self.pause = pause;
        self
    }

    /// Add custom Chrome capability setup applied to every `WebDriver` session.
    ///
    /// The runner applies its own visible/headless configuration first, then applies custom setup
    /// functions in the order they were added. The setup function must be thread-safe because
    /// parallel browser tests can create multiple sessions at the same time.
    #[must_use]
    pub fn with_chrome_capabilities(
        mut self,
        setup: impl Fn(&mut ChromeCapabilities) -> WebDriverResult<()> + Send + Sync + 'static,
    ) -> Self {
        self.chrome_capabilities_setups.push(Arc::new(setup));
        self
    }

    /// Set the `WebDriver` timeouts of every session.
    ///
    /// Individual [`crate::BrowserTest`] implementations can override this by returning `Some` from
    /// [`crate::BrowserTest::timeouts`].
    #[must_use]
    pub const fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = Some(timeouts);
        self
    }

    /// Set the element query wait of every session.
    ///
    /// Individual [`crate::BrowserTest`] implementations can override this by returning `Some` from
    /// [`crate::BrowserTest::element_query_wait`].
    #[must_use]
    pub const fn with_element_query_wait(mut self, wait: ElementQueryWait) -> Self {
        self.element_query_wait = Some(wait);
        self
    }

    /// Set how test failures affect the rest of the run. Defaults to [`FailurePolicy::FailFast`].
    #[must_use]
    pub const fn with_failure_policy(mut self, failure_policy: FailurePolicy) -> Self {
        self.failure_policy = failure_policy;
        self
    }

    /// Capture recent browser-driver output and attach it to the errors of failed runs and tests.
    /// Defaults to [`DriverOutput::disabled`].
    #[must_use]
    pub const fn with_driver_output(mut self, driver_output: DriverOutput) -> Self {
        self.driver_output = driver_output;
        self
    }

    /// Set the cache directory used for Chrome-for-Testing downloads.
    ///
    /// By default, `chrome-for-testing-manager` uses the platform's per-user cache directory.
    /// Pinning this to a project-local directory is useful in sandboxed or CI environments where
    /// the user cache is not writable or where executing browser bundles from outside the project
    /// is restricted.
    #[must_use]
    pub fn with_chrome_for_testing_cache_dir(mut self, cache_dir: impl Into<PathBuf>) -> Self {
        self.chrome_for_testing_cache_dir = Some(cache_dir.into());
        self
    }

    /// Select the browser binary used for headless runs. Defaults to [`ChromeBinary::Chrome`].
    ///
    /// Visible runs always use regular Chrome because Chrome Headless Shell cannot show an
    /// interactive debugging window.
    #[must_use]
    pub const fn with_headless_chrome_binary(mut self, chrome_binary: ChromeBinary) -> Self {
        self.headless_chrome_binary = chrome_binary;
        self
    }

    /// Set how many `WebDriver` sessions the runner creates ahead of the tests that use them.
    ///
    /// Every test runs in a fresh session. Creating one starts a new browser, so the runner
    /// creates the sessions of the next tests while earlier tests run, and a test usually finds its
    /// session ready when its turn comes. Sessions are quit in the background after their test.
    ///
    /// Defaults to the number of tests that can run at the same time (see
    /// [`BrowserTests::parallel`]): one spare session per running test. Up to `parallel tests + spare sessions` browsers are open at once,
    /// so lower this on machines with little memory. `0` creates each session only when its test
    /// is about to run. In visible runs, the spare sessions' browser windows open ahead of their
    /// tests.
    #[must_use]
    pub const fn with_spare_sessions(mut self, spare_sessions: usize) -> Self {
        self.spare_sessions = Some(spare_sessions);
        self
    }

    /// Configure when the runner warns that tests do not progress fast enough.
    ///
    /// Defaults to [`ProgressWarnings::default`].
    #[must_use]
    pub const fn with_progress_warnings(mut self, progress_warnings: ProgressWarnings) -> Self {
        self.progress_warnings = progress_warnings;
        self
    }

    /// Add a consumer of the report of every run, e.g. [`crate::StderrSummary`] to print a
    /// summary of where the run spent its time.
    ///
    /// Consumers are called in the order they were added. Without a consumer, the report is
    /// neither printed nor logged. See [`RunReportConsumer`].
    #[must_use]
    pub fn with_report_consumer(mut self, consumer: impl RunReportConsumer + 'static) -> Self {
        self.report_consumers.push(Arc::new(consumer));
        self
    }

    /// Run every test with a fresh `WebDriver` session.
    ///
    /// Sessions are created ahead of the tests that use them, see [`Self::with_spare_sessions`].
    ///
    /// The shared chromedriver process is always terminated, even when a test returns an error or
    /// panics. Test panics are converted into [`BrowserTestError::Panic`] reports instead of being
    /// resumed.
    ///
    /// `tests` decides which tests run one after another and which at the same time, see
    /// [`BrowserTests`]. The run stops on the first failure by default; use
    /// [`Self::with_failure_policy`] to run every test and return all failures as child reports on
    /// one aggregate report.
    ///
    /// Logs each test's timing when it finishes. If tests ran, the run's report is handed to the
    /// consumers added with [`Self::with_report_consumer`].
    ///
    /// Non-empty runs require a multithreaded Tokio runtime because [`ChromeForTesting::launch`]
    /// requires one. Use `#[tokio::test(flavor = "multi_thread")]` for browser tests.
    ///
    /// # Parameters
    ///
    /// `context`: Given to each test.
    ///
    /// # Errors
    ///
    /// Returns an error if Chrome for Testing cannot be launched or shut down, if a session cannot
    /// be created, or if any test fails.
    pub async fn run<Context, TestError>(
        &self,
        context: &Context,
        tests: BrowserTests<Context, TestError>,
    ) -> Result<(), Report<BrowserTestError>>
    where
        Context: Sync + ?Sized,
        TestError: ?Sized + 'static,
    {
        let start = Instant::now();
        let mut report = BrowserTestRunReport::default();
        let result = self.run_reporting(context, tests, &mut report).await;
        report.total = start.elapsed();
        if !report.tests.is_empty() {
            for consumer in &self.report_consumers {
                consumer.consume(&report);
            }
        }
        result
    }

    async fn run_reporting<Context, TestError>(
        &self,
        context: &Context,
        tests: BrowserTests<Context, TestError>,
        report: &mut BrowserTestRunReport,
    ) -> Result<(), Report<BrowserTestError>>
    where
        Context: Sync + ?Sized,
        TestError: ?Sized + 'static,
    {
        if tests.is_empty() {
            tracing::info!("Skipping browser test run because no tests were provided.");
            return Ok(());
        }

        if pause::pause_if_requested(&self.pause).await? == PauseDecision::Abort {
            tracing::info!("Browser test run aborted at manual pause.");
            return Ok(());
        }

        tracing::info!("Launching Chrome for Testing...");
        let startup_start = Instant::now();
        // A launch failure carries the driver's recent output itself.
        let chrome = ChromeForTesting::launch(
            ChromeForTestingConfig::builder()
                .version(VersionRequest::LatestIn(self.channel.clone()))
                .chrome_binary(self.chrome_binary_for_run())
                .cache_dir_opt(self.chrome_for_testing_cache_dir.clone())
                .build(),
        )
        .await
        .context(BrowserTestError::LaunchChromeForTesting)?;
        report.webdriver_startup = startup_start.elapsed();
        let driver_output = self.driver_output_capture_for_run();
        let output_follower = driver_output
            .as_ref()
            .map(|capture| capture.follow(&chrome));

        let execution = self.run_tests(&chrome, context, tests).await;
        report.tests = execution.records;
        report.groups = execution.groups;
        let test_result = execution.result;

        let shutdown_start = Instant::now();
        let shutdown_result = chrome
            .shutdown()
            .await
            .map(|_exit_status| ())
            .context(BrowserTestError::ShutDownChromeForTesting);
        report.webdriver_shutdown = shutdown_start.elapsed();
        if let Some(output_follower) = output_follower {
            output_follower.finish().await;
        }

        let result = match shutdown_result {
            Ok(()) => test_result,
            Err(shutdown_error) => merge_shutdown_result(test_result, shutdown_error),
        };
        attach_browser_driver_output_to_result(result, driver_output.as_ref())
    }

    fn chrome_binary_for_run(&self) -> ChromeBinary {
        if self.visibility.is_visible() {
            ChromeBinary::Chrome
        } else {
            self.headless_chrome_binary
        }
    }

    async fn run_tests<Context, TestError>(
        &self,
        chrome: &ChromeForTesting,
        context: &Context,
        tests: BrowserTests<Context, TestError>,
    ) -> Execution
    where
        Context: Sync + ?Sized,
        TestError: ?Sized + 'static,
    {
        let config = ExecutionConfig {
            chrome,
            visible: self.visibility.is_visible(),
            timeouts: self.timeouts.as_ref(),
            element_query_wait: self.element_query_wait.as_ref(),
            chrome_capabilities_setups: &self.chrome_capabilities_setups,
            failure_policy: self.failure_policy,
            progress_warnings: self.progress_warnings,
            spare_sessions: self.spare_sessions,
        };
        execute_tests(&config, context, tests).await
    }

    fn driver_output_capture_for_run(&self) -> Option<DriverOutputCapture> {
        self.driver_output
            .tail_line_count()
            .map(DriverOutputCapture::new)
    }
}

fn merge_shutdown_result(
    test_result: Result<(), Report<BrowserTestError>>,
    shutdown_error: Report<BrowserTestError>,
) -> Result<(), Report<BrowserTestError>> {
    let Err(mut test_error) = test_result else {
        return Err(shutdown_error);
    };

    tracing::error!(
        "Failed to shut down Chrome for Testing after browser test failure: {shutdown_error:?}"
    );

    test_error
        .children_mut()
        .push(shutdown_error.into_dynamic().into_cloneable());
    Err(test_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvVarGuard;
    use assertr::prelude::*;
    use chrome_for_testing_manager::{DriverOutputLine, DriverOutputSource};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use thirtyfour::ChromiumLikeCapabilities;

    #[test]
    fn runner_defaults_to_sequential_fail_fast_headless_execution() {
        let runner = BrowserTestRunner::new();

        assert_that!(runner.failure_policy).is_equal_to(FailurePolicy::FailFast);
        assert_that!(runner.visibility).is_equal_to(Visibility::Headless);
        assert_that!(runner.pause.is_enabled()).is_false();
        assert_that!(runner.driver_output_capture_for_run().is_none()).is_true();
    }

    #[test]
    fn runner_builders_set_their_settings() {
        let timeouts = Timeouts::builder()
            .script_timeout(Duration::from_secs(10))
            .build();
        let wait = ElementQueryWait::new(Duration::from_secs(10), Duration::from_millis(500))
            .expect("non-zero interval is valid");

        let runner = BrowserTestRunner::new()
            .with_failure_policy(FailurePolicy::RunAll)
            .with_visibility(Visibility::Visible)
            .with_pause(Pause::enabled())
            .with_timeouts(timeouts)
            .with_element_query_wait(wait)
            .with_chrome_capabilities(|caps| caps.add_arg("--no-sandbox"))
            .with_chrome_for_testing_cache_dir("/tmp/browser-test-cft-cache");

        assert_that!(runner.failure_policy).is_equal_to(FailurePolicy::RunAll);
        assert_that!(runner.visibility).is_equal_to(Visibility::Visible);
        assert_that!(runner.pause.is_enabled()).is_true();
        assert_that!(runner.timeouts).is_equal_to(Some(timeouts));
        assert_that!(runner.element_query_wait).is_equal_to(Some(wait));
        assert_that!(runner.chrome_capabilities_setups.len()).is_equal_to(1);
        assert_that!(runner.chrome_for_testing_cache_dir)
            .is_equal_to(Some(PathBuf::from("/tmp/browser-test-cft-cache")));
    }

    #[test]
    fn visibility_reads_env() {
        let env = EnvVarGuard::new(DEFAULT_VISIBLE_ENV);
        env.set("1");
        assert_that!(Visibility::from_env()).is_equal_to(Ok(Some(Visibility::Visible)));

        env.set("0");
        assert_that!(Visibility::from_env()).is_equal_to(Ok(Some(Visibility::Headless)));

        env.remove();
        assert_that!(Visibility::from_env()).is_equal_to(Ok(None));

        env.set("visible");
        assert_that!(Visibility::from_env().is_err()).is_true();
    }

    #[test]
    fn visibility_reads_custom_env_var() {
        let env = EnvVarGuard::new("BROWSER_TEST_CUSTOM_VISIBLE");
        env.set("yes");

        assert_that!(Visibility::from_env_var("BROWSER_TEST_CUSTOM_VISIBLE"))
            .is_equal_to(Ok(Some(Visibility::Visible)));
    }

    #[test]
    fn headless_chrome_binary_is_used_only_in_headless_runs() {
        let runner =
            BrowserTestRunner::new().with_headless_chrome_binary(ChromeBinary::ChromeHeadlessShell);
        assert_that!(runner.chrome_binary_for_run()).is_equal_to(ChromeBinary::ChromeHeadlessShell);

        let runner = runner.with_visibility(Visibility::Visible);
        assert_that!(runner.chrome_binary_for_run()).is_equal_to(ChromeBinary::Chrome);
    }

    #[test]
    fn driver_output_creates_a_fresh_capture_per_run() {
        let runner = BrowserTestRunner::new().with_driver_output(DriverOutput::tail_lines(1));

        let first = runner
            .driver_output_capture_for_run()
            .expect("tail-line capture should be enabled");
        let second = runner
            .driver_output_capture_for_run()
            .expect("tail-line capture should be enabled");
        first.push(DriverOutputLine::new(
            DriverOutputSource::Stdout,
            "first run",
        ));

        assert_that!(first.snapshot().total_lines).is_equal_to(1);
        assert_that!(second.snapshot().total_lines).is_equal_to(0);
    }

    #[test]
    fn driver_output_of_zero_lines_creates_no_capture() {
        let runner = BrowserTestRunner::new().with_driver_output(DriverOutput::tail_lines(0));

        assert_that!(runner.driver_output_capture_for_run().is_none()).is_true();
    }

    #[tokio::test]
    async fn report_consumers_are_not_called_for_runs_without_tests() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let runner =
            BrowserTestRunner::new().with_report_consumer(move |_report: &BrowserTestRunReport| {
                counter.fetch_add(1, Ordering::SeqCst);
            });

        let result = runner.run(&(), BrowserTests::<()>::sequential()).await;

        assert_that!(result.is_ok()).is_true();
        assert_that!(calls.load(Ordering::SeqCst)).is_equal_to(0);
    }

    #[test]
    fn shutdown_error_is_attached_to_a_test_failure() {
        let test_error = Report::new(BrowserTestError::RunTest {
            test_name: "login".to_owned(),
        });
        let shutdown_error = Report::new(BrowserTestError::ShutDownChromeForTesting);

        let error = merge_shutdown_result(Err(test_error), shutdown_error)
            .expect_err("a failed test stays an error");

        assert_that!(error.current_context().clone()).is_equal_to(BrowserTestError::RunTest {
            test_name: "login".to_owned(),
        });
        assert_that!(error.children().len()).is_equal_to(1);
    }
}

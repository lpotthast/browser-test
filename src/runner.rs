use std::{
    fmt::{self, Display},
    num::NonZeroUsize,
    sync::Arc,
};

use chrome_for_testing_manager::{
    Channel, Chromedriver, ChromedriverRunConfig, DriverOutputListener, PortRequest, VersionRequest,
};
use rootcause::Report;
use rootcause::prelude::ResultExt;
use thirtyfour::{ChromeCapabilities, error::WebDriverResult};

use crate::driver_output::{
    DriverOutputCapture, DriverOutputConfig, attach_browser_driver_output,
    attach_browser_driver_output_to_result, browser_driver_output_config_from_env,
};
use crate::env::env_flag_enabled;
use crate::execution::{ChromeCapabilitiesSetup, browser_test_executions};
use crate::pause::{self, PauseConfig, PauseDecision};
use crate::scheduler::{
    BrowserTestFailurePolicy, BrowserTestParallelism, run_test_executions_parallel,
    run_test_executions_sequential,
};
use crate::{BrowserTestError, BrowserTests, BrowserTimeouts, ElementQueryWaitConfig};

pub(crate) const DEFAULT_VISIBLE_ENV: &str = "BROWSER_TEST_VISIBLE";

/// Chrome visibility mode for [`BrowserTestRunner`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum BrowserTestVisibility {
    /// Run Chrome headlessly.
    #[default]
    Headless,

    /// Run Chrome visibly.
    Visible,

    /// Read visibility from the given environment variable.
    ///
    /// The variable is considered enabled unless it is unset, empty, `0`, `false`, `no`, or `off`.
    FromEnvVar(String),

    /// Read visibility from `BROWSER_TEST_VISIBLE`.
    FromEnv,
}

impl BrowserTestVisibility {
    /// Build a headless visibility config.
    #[must_use]
    pub const fn headless() -> Self {
        Self::Headless
    }

    /// Build a visible visibility config.
    #[must_use]
    pub const fn visible() -> Self {
        Self::Visible
    }

    /// Build a visibility config from `BROWSER_TEST_VISIBLE`.
    #[must_use]
    pub const fn from_env() -> Self {
        Self::FromEnv
    }

    /// Build a visibility config from an environment variable.
    #[must_use]
    pub fn from_env_var(env_var: impl Into<String>) -> Self {
        Self::FromEnvVar(env_var.into())
    }

    fn is_visible(&self) -> bool {
        match self {
            Self::Headless => false,
            Self::Visible => true,
            Self::FromEnvVar(env_var) => env_flag_enabled(env_var),
            Self::FromEnv => env_flag_enabled(DEFAULT_VISIBLE_ENV),
        }
    }
}

/// Runs [`crate::BrowserTest`] implementations through Chrome for Testing.
#[derive(Clone)]
pub struct BrowserTestRunner {
    channel: Channel,
    visible: bool,
    pause: Option<PauseConfig>,
    hint: Option<String>,
    parallelism: BrowserTestParallelism,
    failure_policy: BrowserTestFailurePolicy,
    webdriver_timeouts: Option<BrowserTimeouts>,
    element_query_wait: Option<ElementQueryWaitConfig>,
    chrome_capabilities_setups: Vec<Arc<ChromeCapabilitiesSetup>>,
    browser_driver_output: BrowserDriverOutputSetting,
}

#[derive(Debug, Clone)]
enum BrowserDriverOutputSetting {
    Disabled,
    TailLines(NonZeroUsize),
}

impl Default for BrowserTestRunner {
    fn default() -> Self {
        Self {
            channel: Channel::Stable,
            visible: false,
            pause: None,
            hint: None,
            parallelism: BrowserTestParallelism::Sequential,
            failure_policy: BrowserTestFailurePolicy::FailFast,
            webdriver_timeouts: None,
            element_query_wait: None,
            chrome_capabilities_setups: Vec::new(),
            browser_driver_output: BrowserDriverOutputSetting::Disabled,
        }
    }
}

impl fmt::Debug for BrowserTestRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserTestRunner")
            .field("channel", &self.channel)
            .field("visible", &self.visible)
            .field("pause", &self.pause)
            .field("hint", &self.hint)
            .field("parallelism", &self.parallelism)
            .field("failure_policy", &self.failure_policy)
            .field("webdriver_timeouts", &self.webdriver_timeouts)
            .field("element_query_wait", &self.element_query_wait)
            .field(
                "chrome_capabilities_setup_count",
                &self.chrome_capabilities_setups.len(),
            )
            .field("browser_driver_output", &self.browser_driver_output)
            .finish()
    }
}

impl BrowserTestRunner {
    /// Create a runner using stable Chrome in headless mode.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Select the Chrome release channel.
    #[must_use]
    pub fn with_channel(mut self, channel: Channel) -> Self {
        self.channel = channel;
        self
    }

    /// Configure Chrome visibility.
    #[must_use]
    pub fn with_visibility(mut self, visibility: impl Into<BrowserTestVisibility>) -> Self {
        self.visible = visibility.into().is_visible();
        self
    }

    /// Pause before starting webdriver when the config is enabled.
    ///
    /// If the pause is aborted, [`Self::run`] returns successfully without starting webdriver or
    /// running any tests. If stdin reaches EOF while waiting for a pause response, [`Self::run`]
    /// returns an error instead.
    #[must_use]
    pub fn with_pause(mut self, pause: impl Into<PauseConfig>) -> Self {
        self.pause = Some(pause.into());
        self
    }

    /// Set extra context shown when a manual pause prompt is enabled.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Display) -> Self {
        self.hint = Some(hint.to_string());
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

    /// Set timeouts applied to every session before running tests.
    ///
    /// Individual [`crate::BrowserTest`] implementations can override this by returning `Some` from
    /// [`crate::BrowserTest::timeouts`].
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: BrowserTimeouts) -> Self {
        self.webdriver_timeouts = Some(timeouts);
        self
    }

    /// Deprecated name for [`Self::with_timeouts`].
    ///
    /// Use [`Self::with_timeouts`] in new code.
    #[deprecated(since = "0.1.0", note = "use with_timeouts instead")]
    #[must_use]
    pub fn with_webdriver_timeouts(self, timeouts: BrowserTimeouts) -> Self {
        self.with_timeouts(timeouts)
    }

    /// Set the element query wait applied to every session before running tests.
    ///
    /// Individual [`crate::BrowserTest`] implementations can override this by returning `Some` from
    /// [`crate::BrowserTest::element_query_wait`].
    #[must_use]
    pub const fn with_element_query_wait(mut self, wait: ElementQueryWaitConfig) -> Self {
        self.element_query_wait = Some(wait);
        self
    }

    /// Configure how browser tests are scheduled.
    #[must_use]
    pub fn with_test_parallelism(mut self, parallelism: impl Into<BrowserTestParallelism>) -> Self {
        self.parallelism = parallelism.into();
        self
    }

    /// Configure how browser test failures affect the rest of the run.
    #[must_use]
    pub fn with_failure_policy(
        mut self,
        failure_policy: impl Into<BrowserTestFailurePolicy>,
    ) -> Self {
        self.failure_policy = failure_policy.into();
        self
    }

    /// Capture recent browser-driver output for failure diagnostics.
    ///
    /// This stores capture configuration and creates a fresh capture buffer for each
    /// [`Self::run`] call.
    #[must_use]
    pub fn with_driver_output(mut self, config: impl Into<DriverOutputConfig>) -> Self {
        self.browser_driver_output = browser_driver_output_setting(config.into());
        self
    }

    /// Deprecated name for [`Self::with_driver_output`].
    ///
    /// Use [`Self::with_driver_output`] in new code.
    #[deprecated(since = "0.1.0", note = "use with_driver_output instead")]
    #[must_use]
    pub fn with_browser_driver_output(self, config: impl Into<DriverOutputConfig>) -> Self {
        self.with_driver_output(config)
    }

    /// Run every test with a fresh `WebDriver` session.
    ///
    /// The shared chromedriver process is always terminated, even when a test returns an error or
    /// panics. Test panics are converted into [`BrowserTestError::Panic`] reports instead of being
    /// resumed.
    ///
    /// Tests run sequentially and stop on the first failure by default. Use
    /// [`Self::with_test_parallelism`] to run multiple fresh `WebDriver` sessions at once. Use
    /// [`Self::with_failure_policy`] to execute every test and return all failures as child reports
    /// on one aggregate report.
    ///
    /// Non-empty runs require a multi-threaded Tokio runtime because
    /// [`Chromedriver::run`] requires one. Use `#[tokio::test(flavor = "multi_thread")]` for
    /// browser tests.
    ///
    /// # Parameters
    ///
    /// `context`: Given to each test.
    ///
    /// # Errors
    ///
    /// Returns an error if chromedriver cannot be started or terminated, if a session cannot be
    /// created, or if any test fails.
    pub async fn run<Context, TestError>(
        &self,
        context: &Context,
        tests: BrowserTests<Context, TestError>,
    ) -> Result<(), Report<BrowserTestError>>
    where
        Context: Sync + ?Sized,
        TestError: ?Sized + 'static,
    {
        if tests.is_empty() {
            tracing::info!("Skipping browser test run because no tests were provided.");
            return Ok(());
        }

        if let Some(pause) = self.pause.clone()
            && pause::pause_if_requested(pause, self.hint.as_deref()).await? == PauseDecision::Abort
        {
            tracing::info!("Browser test run aborted at manual pause.");
            return Ok(());
        }

        tracing::info!("Starting webdriver...");
        let browser_driver_output = self.browser_driver_output_capture_for_run();
        let output_listener: Option<DriverOutputListener> = browser_driver_output
            .as_ref()
            .map(DriverOutputCapture::listener);
        let chromedriver = match Chromedriver::run(
            ChromedriverRunConfig::builder()
                .version(VersionRequest::LatestIn(self.channel.clone()))
                .port(PortRequest::Any)
                .output_listener_opt(output_listener)
                .build(),
        )
        .await
        .context(BrowserTestError::StartWebdriver)
        {
            Ok(chromedriver) => chromedriver,
            Err(mut err) => {
                attach_browser_driver_output(&mut err, browser_driver_output.as_ref());
                return Err(err);
            }
        };

        let test_result = self.run_tests(&chromedriver, context, tests).await;

        let termination_result = chromedriver
            .terminate()
            .await
            .context(BrowserTestError::TerminateWebdriver);

        if let Err(err) = termination_result {
            return attach_browser_driver_output_to_result(
                merge_termination_result(test_result, err),
                browser_driver_output.as_ref(),
            );
        }

        attach_browser_driver_output_to_result(test_result, browser_driver_output.as_ref())
    }

    /// Runs `tests` while respecting this runner's `parallelism` configuration.
    async fn run_tests<Context, TestError>(
        &self,
        chromedriver: &Chromedriver,
        context: &Context,
        tests: BrowserTests<Context, TestError>,
    ) -> Result<(), Report<BrowserTestError>>
    where
        Context: Sync + ?Sized,
        TestError: ?Sized + 'static,
    {
        let max_parallel_tests = self.parallelism.max_parallel_tests();
        let executions = browser_test_executions(
            chromedriver,
            self.visible,
            self.webdriver_timeouts.as_ref(),
            self.element_query_wait.as_ref(),
            &self.chrome_capabilities_setups,
            context,
            tests,
        );

        if max_parallel_tests.get() == 1 {
            run_test_executions_sequential(self.failure_policy, executions).await
        } else {
            run_test_executions_parallel(self.failure_policy, executions, max_parallel_tests).await
        }
    }

    fn browser_driver_output_capture_for_run(&self) -> Option<DriverOutputCapture> {
        match &self.browser_driver_output {
            BrowserDriverOutputSetting::Disabled => None,
            BrowserDriverOutputSetting::TailLines(tail_lines) => {
                Some(DriverOutputCapture::new(*tail_lines))
            }
        }
    }
}

fn browser_driver_output_setting(config: DriverOutputConfig) -> BrowserDriverOutputSetting {
    match config {
        DriverOutputConfig::Disabled => BrowserDriverOutputSetting::Disabled,
        DriverOutputConfig::TailLines(tail_lines) => NonZeroUsize::new(tail_lines).map_or(
            BrowserDriverOutputSetting::Disabled,
            BrowserDriverOutputSetting::TailLines,
        ),
        DriverOutputConfig::FromEnv => {
            browser_driver_output_setting(browser_driver_output_config_from_env())
        }
    }
}

fn merge_termination_result(
    test_result: Result<(), Report<BrowserTestError>>,
    termination_error: Report<BrowserTestError>,
) -> Result<(), Report<BrowserTestError>> {
    let Err(mut test_error) = test_result else {
        return Err(termination_error);
    };

    tracing::error!(
        "Failed to terminate chromedriver after browser test failure: {termination_error:?}"
    );

    test_error
        .children_mut()
        .push(termination_error.into_dynamic().into_cloneable());
    Err(test_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_output::DEFAULT_BROWSER_DRIVER_OUTPUT_ENV;
    use crate::driver_output::DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES;
    use crate::driver_output::DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES_ENV;
    use crate::test_support::EnvVarGuard;
    use assertr::prelude::*;
    use chrome_for_testing_manager::{DriverOutputLine, DriverOutputSource};
    use std::env;
    use std::time::Duration;
    use thirtyfour::ChromiumLikeCapabilities;

    #[test]
    fn runner_defaults_to_sequential_fail_fast_execution() {
        let runner = BrowserTestRunner::new();

        assert_that!(runner.parallelism).is_equal_to(BrowserTestParallelism::Sequential);
        assert_that!(runner.failure_policy).is_equal_to(BrowserTestFailurePolicy::FailFast);
    }

    #[test]
    fn runner_parallelism_builders_set_scheduling_mode() {
        let max_parallel_tests =
            NonZeroUsize::new(3).expect("literal parallelism should be non-zero");

        let runner = BrowserTestRunner::new()
            .with_test_parallelism(BrowserTestParallelism::Parallel(max_parallel_tests));
        assert_that!(runner.parallelism)
            .is_equal_to(BrowserTestParallelism::Parallel(max_parallel_tests));

        let runner = runner.with_test_parallelism(BrowserTestParallelism::Sequential);
        assert_that!(runner.parallelism).is_equal_to(BrowserTestParallelism::Sequential);
    }

    #[test]
    fn runner_failure_policy_builders_set_failure_mode() {
        let runner = BrowserTestRunner::new().with_failure_policy(BrowserTestFailurePolicy::RunAll);
        assert_that!(runner.failure_policy).is_equal_to(BrowserTestFailurePolicy::RunAll);

        let runner = runner.with_failure_policy(BrowserTestFailurePolicy::FailFast);
        assert_that!(runner.failure_policy).is_equal_to(BrowserTestFailurePolicy::FailFast);
    }

    #[test]
    fn runner_visibility_builder_sets_visible_mode() {
        let runner = BrowserTestRunner::new().with_visibility(BrowserTestVisibility::Visible);
        assert_that!(runner.visible).is_true();

        let runner = runner.with_visibility(BrowserTestVisibility::Headless);
        assert_that!(runner.visible).is_false();
    }

    #[test]
    fn runner_visibility_builder_reads_default_env() {
        let env = EnvVarGuard::new(DEFAULT_VISIBLE_ENV);
        env.set("yes");

        let runner = BrowserTestRunner::new().with_visibility(BrowserTestVisibility::from_env());

        assert_that!(runner.visible).is_true();
    }

    #[test]
    fn runner_browser_driver_output_builder_sets_capture() {
        let runner =
            BrowserTestRunner::new().with_driver_output(DriverOutputConfig::tail_lines(12));

        let BrowserDriverOutputSetting::TailLines(tail_lines) = runner.browser_driver_output else {
            panic!("browser driver output tail-line capture should be configured");
        };
        assert_that!(tail_lines.get()).is_equal_to(12);
    }

    #[allow(deprecated)]
    #[test]
    fn deprecated_browser_driver_output_builder_sets_capture() {
        let runner = BrowserTestRunner::new()
            .with_browser_driver_output(crate::BrowserDriverOutputConfig::new(12));

        let BrowserDriverOutputSetting::TailLines(tail_lines) = runner.browser_driver_output else {
            panic!("browser driver output tail-line capture should be configured");
        };
        assert_that!(tail_lines.get()).is_equal_to(12);
    }

    #[test]
    fn runner_browser_driver_output_zero_tail_disables_capture() {
        let runner = BrowserTestRunner::new().with_driver_output(DriverOutputConfig::tail_lines(0));

        assert_that!(matches!(
            runner.browser_driver_output,
            BrowserDriverOutputSetting::Disabled
        ))
        .is_true();
    }

    #[test]
    fn browser_driver_output_from_env_uses_default_tail_lines() {
        let env = EnvVarGuard::new(DEFAULT_BROWSER_DRIVER_OUTPUT_ENV);
        let original_tail = env::var_os(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES_ENV);
        env.set("1");
        // SAFETY: `env` holds the crate's environment lock for this test.
        unsafe {
            env::remove_var(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES_ENV);
        }

        let runner = BrowserTestRunner::new().with_driver_output(DriverOutputConfig::from_env());

        let BrowserDriverOutputSetting::TailLines(tail_lines) = runner.browser_driver_output else {
            panic!("env browser driver output tail-line capture should be configured");
        };
        assert_that!(tail_lines.get()).is_equal_to(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES);

        // SAFETY: `env` holds the crate's environment lock for this test.
        unsafe {
            match original_tail {
                Some(value) => env::set_var(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES_ENV, value),
                None => env::remove_var(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES_ENV),
            }
        }
    }

    #[test]
    fn browser_driver_output_tail_lines_creates_fresh_capture_per_run() {
        let runner = BrowserTestRunner::new().with_driver_output(DriverOutputConfig::tail_lines(1));

        let first = runner
            .browser_driver_output_capture_for_run()
            .expect("tail-line capture should be enabled");
        let second = runner
            .browser_driver_output_capture_for_run()
            .expect("tail-line capture should be enabled");

        first.push(DriverOutputLine {
            source: DriverOutputSource::Stdout,
            sequence: 0,
            line: "first run".to_owned(),
        });

        assert_that!(first.snapshot().total_lines).is_equal_to(1);
        assert_that!(second.snapshot().total_lines).is_equal_to(0);
    }

    #[test]
    fn browser_driver_output_disabled_creates_no_capture_for_run() {
        let runner = BrowserTestRunner::new().with_driver_output(DriverOutputConfig::disabled());

        assert_that!(runner.browser_driver_output_capture_for_run().is_none()).is_true();
    }

    #[test]
    fn runner_chrome_capabilities_builder_adds_setup() {
        let runner =
            BrowserTestRunner::new().with_chrome_capabilities(|caps| caps.add_arg("--no-sandbox"));

        assert_that!(runner.chrome_capabilities_setups.len()).is_equal_to(1);
    }

    #[test]
    fn runner_webdriver_timeouts_builder_sets_default_timeouts() {
        let timeouts = BrowserTimeouts::builder()
            .script_timeout(Duration::from_secs(10))
            .page_load_timeout(Duration::from_secs(10))
            .implicit_wait_timeout(Duration::from_secs(0))
            .build();

        let runner = BrowserTestRunner::new().with_timeouts(timeouts);

        assert_that!(runner.webdriver_timeouts).is_equal_to(Some(timeouts));
    }

    #[allow(deprecated)]
    #[test]
    fn deprecated_webdriver_timeouts_builder_sets_default_timeouts() {
        let timeouts = BrowserTimeouts::builder()
            .script_timeout(Duration::from_secs(10))
            .page_load_timeout(Duration::from_secs(10))
            .implicit_wait_timeout(Duration::from_secs(0))
            .build();

        let runner = BrowserTestRunner::new().with_webdriver_timeouts(timeouts);

        assert_that!(runner.webdriver_timeouts).is_equal_to(Some(timeouts));
    }

    #[test]
    fn runner_element_query_wait_builder_sets_default_wait() {
        let wait = ElementQueryWaitConfig::builder()
            .timeout(Duration::from_secs(10))
            .interval(Duration::from_millis(500))
            .build();

        let runner = BrowserTestRunner::new().with_element_query_wait(wait);

        assert_that!(runner.element_query_wait).is_equal_to(Some(wait));
    }

    #[test]
    fn runner_with_no_tests_returns_without_starting_webdriver_or_pausing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime should build");

        runtime.block_on(async {
            BrowserTestRunner::new()
                .with_pause(PauseConfig::enabled(true))
                .run(&(), BrowserTests::<()>::new())
                .await
                .expect("empty test runs should be a no-op");
        });
    }

    #[test]
    fn termination_failure_is_attached_to_existing_test_failure() {
        let test_result = Err(Report::new(BrowserTestError::RunTest {
            test_name: "login".to_owned(),
        }));
        let termination_error = Report::new(BrowserTestError::TerminateWebdriver);

        let err = merge_termination_result(test_result, termination_error)
            .expect_err("test and termination failure should fail");

        assert_that!(err.to_string()).contains(
            BrowserTestError::RunTest {
                test_name: "login".to_owned(),
            }
            .to_string(),
        );
        assert_that!(err.children().len()).is_equal_to(1);
    }
}

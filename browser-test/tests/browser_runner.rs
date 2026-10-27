//! Integration tests for public browser runner behavior.

use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use assertr::prelude::*;
use browser_test::{
    BrowserTest, BrowserTestError, BrowserTestRunReport, BrowserTestRunner, Cancellation,
    CancellationToken, FailurePolicy, Parallelism, StepExt, TestFilter, TestOutcome, TestSettings,
    TestSuite, async_trait, browser_test,
    thirtyfour::{By, ChromiumLikeCapabilities, WebDriver, prelude::ElementQueryable},
};
use rootcause::{Report, prelude::ResultExt};
use serial_test::serial;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

type RunnerResult = Result<(), Report<BrowserTestError>>;

const FIXTURE_PAGE_URL: &str =
    "data:text/html,%3C!doctype%20html%3E%3Ctitle%3Ebrowser-test%20fixture%3C/title%3E";
const FIXTURE_PAGE_TITLE: &str = "browser-test fixture";

/// Set for a child run (see [`ChildRun`]): the directory in which it keeps its profiles and
/// signals that its test is running.
const CHILD_RUN_DIR_ENV: &str = "BROWSER_TEST_CHILD_RUN_DIR";

#[derive(Debug)]
struct IntegrationContext {
    page_url: &'static str,
    expected_title: &'static str,
}

impl Default for IntegrationContext {
    fn default() -> Self {
        Self {
            page_url: FIXTURE_PAGE_URL,
            expected_title: FIXTURE_PAGE_TITLE,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum IntegrationTestError {
    #[error("failed to open test page")]
    OpenTestPage,

    #[error("failed to read browser title")]
    ReadTitle,

    #[error("unexpected browser title: expected {expected:?}, got {actual:?}")]
    UnexpectedTitle {
        expected: &'static str,
        actual: String,
    },

    #[error("intentional browser test failure")]
    IntentionalFailure,

    #[error("failed to freeze the page")]
    FreezePage,

    #[error("failed to open a dialog")]
    OpenDialog,

    #[error("failed to focus an element from script")]
    FocusElement,

    #[error("failed to find an element")]
    FindElement,

    #[error("focusing an element from script fired no `focus` event: the page lacks focus")]
    NoFocusEvent,
}

struct PageTitleTest {
    name: String,
    started: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl BrowserTest<IntegrationContext> for PageTitleTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.name.as_str())
    }

    async fn run(&self, driver: &WebDriver, context: &IntegrationContext) -> Result<(), Report> {
        if let Some(started) = &self.started {
            started.fetch_add(1, Ordering::SeqCst);
        }

        driver
            .goto(context.page_url)
            .await
            .context(IntegrationTestError::OpenTestPage)?;
        let title = driver
            .title()
            .await
            .context(IntegrationTestError::ReadTitle)?;

        if title != context.expected_title {
            rootcause::bail!(IntegrationTestError::UnexpectedTitle {
                expected: context.expected_title,
                actual: title,
            });
        }

        Ok(())
    }
}

/// Focuses an element from script and expects its `focus` event, which Chrome only fires while the
/// page has focus.
struct FocusEventTest;

#[async_trait]
impl BrowserTest<IntegrationContext> for FocusEventTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("focus event")
    }

    async fn run(&self, driver: &WebDriver, context: &IntegrationContext) -> Result<(), Report> {
        driver
            .goto(context.page_url)
            .await
            .context(IntegrationTestError::OpenTestPage)?;
        let fired: bool = driver
            .execute(
                "const input = document.createElement('input');
                 document.body.append(input);
                 let fired = false;
                 input.addEventListener('focus', () => fired = true);
                 input.focus();
                 return fired;",
                vec![],
            )
            .await
            .context(IntegrationTestError::FocusElement)?
            .convert()
            .context(IntegrationTestError::FocusElement)?;
        if !fired {
            rootcause::bail!(IntegrationTestError::NoFocusEvent);
        }
        Ok(())
    }
}

/// Looks up an element the fixture page doesn't have, in a helper, as a page object would.
struct MissingElementTest;

/// A helper failing on behalf of its caller.
async fn find_missing(driver: &WebDriver) -> Result<(), Report<IntegrationTestError>> {
    driver
        .query(By::Css("#missing"))
        .nowait()
        .first()
        .step("find")
        .detail("#missing")
        .await
        .context(IntegrationTestError::FindElement)?;
    Ok(())
}

#[async_trait]
impl BrowserTest<IntegrationContext> for MissingElementTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("missing element")
    }

    async fn run(&self, driver: &WebDriver, context: &IntegrationContext) -> Result<(), Report> {
        driver
            .goto(context.page_url)
            .step("goto")
            .await
            .context(IntegrationTestError::OpenTestPage)?;
        Ok(find_missing(driver).await?)
    }
}

/// Panics, as a failed assertion does.
struct PanickingTest;

#[async_trait]
impl BrowserTest<IntegrationContext> for PanickingTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("panicking")
    }

    async fn run(&self, _driver: &WebDriver, _context: &IntegrationContext) -> Result<(), Report> {
        assert_that!(1).is_equal_to(2);
        Ok(())
    }
}

/// Panics in a dependency (here `core`, adding durations that overflow), as a library raising an
/// assertion from its own code does.
struct PanickingInADependencyTest;

#[async_trait]
impl BrowserTest<IntegrationContext> for PanickingInADependencyTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("panicking in a dependency")
    }

    async fn run(&self, _driver: &WebDriver, _context: &IntegrationContext) -> Result<(), Report> {
        let _ = std::hint::black_box(std::time::Duration::MAX) + std::time::Duration::from_secs(1);
        Ok(())
    }
}

/// Opens the fixture page, calls `on_running`, then runs until it is cancelled or its process is
/// killed.
struct RunForeverTest {
    on_running: Box<dyn Fn() + Send + Sync>,
}

impl RunForeverTest {
    fn new(on_running: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            on_running: Box::new(on_running),
        }
    }
}

#[async_trait]
impl BrowserTest<IntegrationContext> for RunForeverTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("run forever")
    }

    async fn run(&self, driver: &WebDriver, context: &IntegrationContext) -> Result<(), Report> {
        driver
            .goto(context.page_url)
            .await
            .context(IntegrationTestError::OpenTestPage)?;
        (self.on_running)();
        std::future::pending().await
    }
}

/// Fails. With `started`, counts itself there, and fails only once another test started too, so
/// that the failure happens while that test runs.
struct IntentionalFailureTest {
    started: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl BrowserTest<IntegrationContext> for IntentionalFailureTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("intentional failure")
    }

    async fn run(&self, _driver: &WebDriver, _context: &IntegrationContext) -> Result<(), Report> {
        if let Some(started) = &self.started {
            started.fetch_add(1, Ordering::SeqCst);
            while started.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }

        Err(rootcause::report!(IntegrationTestError::IntentionalFailure).into())
    }
}

struct PanicTest {
    started: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl BrowserTest<IntegrationContext> for PanicTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("intentional panic")
    }

    async fn run(&self, _driver: &WebDriver, _context: &IntegrationContext) -> Result<(), Report> {
        if let Some(started) = &self.started {
            started.fetch_add(1, Ordering::SeqCst);
        }

        panic!("intentional browser test panic");
    }
}

#[derive(Clone, Copy)]
enum MetadataPanicHook {
    Name,
    Tags,
    Settings,
}

struct MetadataPanicTest {
    panic_in: MetadataPanicHook,
}

#[async_trait]
impl BrowserTest<IntegrationContext> for MetadataPanicTest {
    fn name(&self) -> Cow<'_, str> {
        if matches!(self.panic_in, MetadataPanicHook::Name) {
            panic!("name hook failed");
        }

        Cow::Borrowed("metadata panic")
    }

    fn tags(&self) -> Vec<Cow<'_, str>> {
        if matches!(self.panic_in, MetadataPanicHook::Tags) {
            panic!("tags hook failed");
        }

        Vec::new()
    }

    fn settings(&self) -> TestSettings {
        if matches!(self.panic_in, MetadataPanicHook::Settings) {
            panic!("settings hook failed");
        }

        TestSettings::new()
    }

    async fn run(&self, _driver: &WebDriver, _context: &IntegrationContext) -> Result<(), Report> {
        Ok(())
    }
}

/// [`runner_with`] runs that are never cancelled.
fn runner() -> BrowserTestRunner {
    runner_with(Cancellation::disabled())
}

/// Build a `BrowserTestRunner` pre-configured with the Chrome flags our CI environment needs. Both
/// flags are harmless when set locally, so we apply them unconditionally instead of branching on
/// `CI`.
fn runner_with(cancellation: Cancellation) -> BrowserTestRunner {
    BrowserTestRunner::new(cancellation)
        .with_chrome_capabilities(|caps| {
            // `--no-sandbox` disables Chrome's child-process sandboxing. User-mode tarball
            // extraction can't set the setuid-root bit on `chrome_sandbox` (the privileged helper
            // Chrome execs to install the sandbox), and CI kernels often also restrict
            // unprivileged user namespaces. Without either layer, Chrome exits before chromedriver
            // opens a session.
            caps.add_arg("--no-sandbox")?;

            // `--disable-dev-shm-usage` makes Chrome place its IPC shared-memory segments under
            // `/tmp` instead of `/dev/shm`. CI runners typically expose `/dev/shm` tiny tmpfs,
            // which Chrome can exhaust during session startup.
            caps.add_arg("--disable-dev-shm-usage")?;
            Ok(())
        })
        .with_failure_policy(FailurePolicy::RunAll)
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn default_sequential_fail_fast_runs_page_title_test() -> RunnerResult {
    runner()
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(page_title_test("page title")),
        )
        .await
}

/// A failure report locates an error created in a helper at the test code that called it, and
/// lists the test's last steps.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn failure_reports_locate_errors_in_test_code() {
    let err = runner()
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(MissingElementTest),
        )
        .await
        .expect_err("the element is missing");

    let report = format!("{err:?}");
    assert_that!(&report)
        .contains("Test code:")
        .contains("tests/browser_runner.rs")
        .contains("browser_runner::find_missing")
        .contains("MissingElementTest")
        .contains("Last steps (time since the test started):")
        .contains("goto")
        .contains("find #missing")
        .does_not_contain("Stacktrace");
}

/// Calls [`find_missing`] from a `#[browser_test]` function.
#[browser_test]
async fn missing_element(
    driver: &WebDriver,
    context: &IntegrationContext,
) -> Result<(), Report<IntegrationTestError>> {
    driver
        .goto(context.page_url)
        .await
        .context(IntegrationTestError::OpenTestPage)?;
    find_missing(driver).await
}

/// The test code of a `#[browser_test]` ends at its function, named as written.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn failure_reports_name_browser_test_functions() {
    let err = runner()
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(MissingElement),
        )
        .await
        .expect_err("the element is missing");

    let report = format!("{err:?}");
    let test_code: Vec<_> = report
        .lines()
        .skip_while(|line| !line.contains("Test code:"))
        .skip(1)
        .take_while(|line| line.contains("tests/browser_runner.rs"))
        .collect();
    assert_that!(test_code).has_length(2);
    assert_that!(test_code[0]).ends_with("browser_runner::find_missing");
    assert_that!(test_code[1]).ends_with("browser_runner::missing_element");
}

/// A failure report locates a panic and its test code.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn failure_reports_locate_panics() {
    let err = runner()
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(PanickingTest),
        )
        .await
        .expect_err("the test panics");

    let report = format!("{err:?}");
    assert_that!(&report)
        .contains("Browser test 'panicking' panicked")
        .contains("Panicked at browser-test/tests/browser_runner.rs:")
        .contains("Test code:");
}

/// A panic in a dependency is reported as outside the test code, which the test code's frames
/// locate.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn failure_reports_name_panics_outside_the_test_code() {
    let err = runner()
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(PanickingInADependencyTest),
        )
        .await
        .expect_err("the test panics");

    let report = format!("{err:?}");
    assert_that!(&report)
        .contains("Panicked outside the test code at ")
        .contains("core/src/time.rs")
        .contains("Test code:")
        .contains("tests/browser_runner.rs");
}

/// Sessions run on profiles browser-test creates, on which `ChromeDriver` starts the page without
/// focus; the runner brings it to the front.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sessions_start_with_a_focused_page() -> RunnerResult {
    runner()
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(FocusEventTest),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn explicit_sequential_runs_page_title_test() -> RunnerResult {
    runner()
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(page_title_test("page title")),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn bounded_parallel_runs_page_title_tests() -> RunnerResult {
    runner()
        .run(
            &IntegrationContext::default(),
            TestSuite::parallel(Parallelism::parallel(2))
                .with(page_title_test(String::from("page title one")))
                .with(page_title_test("page title two")),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_all_runs_successful_page_title_tests() -> RunnerResult {
    runner()
        .with_failure_policy(FailurePolicy::RunAll)
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(page_title_test("page title one"))
                .with(page_title_test("page title two")),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_all_reports_intentional_failure_and_runs_page_title_test() {
    let err = runner()
        .with_failure_policy(FailurePolicy::RunAll)
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(IntentionalFailureTest { started: None })
                .with(page_title_test("page title")),
        )
        .await
        .expect_err("run-all should report the intentional failure");

    assert_that!(err.to_string())
        .contains(BrowserTestError::RunTests { failed_tests: 1 }.to_string());
    assert_that!(err).has_child_count(1);
}

/// Implements `run` by hand, and panics before returning its future.
struct PanicsBeforeItsFuture;

impl BrowserTest<IntegrationContext> for PanicsBeforeItsFuture {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("panics before its future")
    }

    fn run<'test, 'driver, 'context, 'future>(
        &'test self,
        _driver: &'driver WebDriver,
        _context: &'context IntegrationContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), Report>> + Send + 'future>>
    where
        'test: 'future,
        'driver: 'future,
        'context: 'future,
        Self: 'future,
    {
        panic!("no future")
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_all_reports_panics_outside_test_bodies_and_runs_remaining_tests() {
    let started = Arc::new(AtomicUsize::new(0));
    let err =
        runner()
            .with_failure_policy(FailurePolicy::RunAll)
            .run(
                &IntegrationContext::default(),
                TestSuite::sequential().with(PanicsBeforeItsFuture).with(
                    page_title_test_with_counter("page title", Arc::clone(&started)),
                ),
            )
            .await
            .expect_err("run-all should report the panic");

    assert_that!(started.load(Ordering::SeqCst)).is_equal_to(1);
    assert_that!(err).has_child_count(1);
    assert_that!(format!("{err:?}")).contains("no future");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_all_reports_metadata_hook_panics_and_runs_remaining_page_title_test() {
    let started = Arc::new(AtomicUsize::new(0));
    let err = runner()
        .with_failure_policy(FailurePolicy::RunAll)
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(MetadataPanicTest {
                    panic_in: MetadataPanicHook::Name,
                })
                .with(MetadataPanicTest {
                    panic_in: MetadataPanicHook::Tags,
                })
                .with(MetadataPanicTest {
                    panic_in: MetadataPanicHook::Settings,
                })
                .with(page_title_test_with_counter(
                    "page title",
                    Arc::clone(&started),
                )),
        )
        .await
        .expect_err("run-all should report metadata hook panics");

    assert_that!(started.load(Ordering::SeqCst)).is_equal_to(1);
    assert_that!(err.to_string())
        .contains(BrowserTestError::RunTests { failed_tests: 3 }.to_string());
    assert_that!(err).has_child_count(3);
    assert_that!(format!("{err:?}")).contains("unnamed test at index 0");
    assert_that!(format!("{err:?}")).contains("tags hook failed");
    assert_that!(format!("{err:?}")).contains("settings hook failed");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn parallel_run_all_reports_panic_and_runs_remaining_page_title_tests() {
    let started = Arc::new(AtomicUsize::new(0));
    let err = runner()
        .with_failure_policy(FailurePolicy::RunAll)
        .run(
            &IntegrationContext::default(),
            TestSuite::parallel(Parallelism::parallel(2))
                .with(PanicTest {
                    started: Some(Arc::clone(&started)),
                })
                .with(page_title_test_with_counter(
                    "page title one",
                    Arc::clone(&started),
                ))
                .with(page_title_test_with_counter(
                    "page title two",
                    Arc::clone(&started),
                )),
        )
        .await
        .expect_err("run-all should report the intentional panic");

    assert_that!(started.load(Ordering::SeqCst)).is_equal_to(3);
    assert_that!(err.to_string())
        .contains(BrowserTestError::RunTests { failed_tests: 1 }.to_string());
    assert_that!(err).has_child_count(1);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn parallel_fail_fast_waits_for_running_page_title_test_without_starting_more() {
    let started = Arc::new(AtomicUsize::new(0));
    let err = runner()
        .with_failure_policy(FailurePolicy::FailFast)
        .run(
            &IntegrationContext::default(),
            TestSuite::parallel(Parallelism::parallel(2))
                .with(IntentionalFailureTest {
                    started: Some(Arc::clone(&started)),
                })
                .with(page_title_test_with_counter(
                    "page title one",
                    Arc::clone(&started),
                ))
                .with(page_title_test_with_counter(
                    "page title two",
                    Arc::clone(&started),
                )),
        )
        .await
        .expect_err("fail-fast should report the intentional failure");

    assert_that!(err.to_string())
        .contains(BrowserTestError::RunTests { failed_tests: 1 }.to_string());
    assert_that!(err).has_child_count(1);
    assert_that!(started.load(Ordering::SeqCst)).is_equal_to(2);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_removes_the_chrome_profiles_of_its_sessions() -> RunnerResult {
    let profiles_dir = ScratchDir::new("removed-profiles");

    runner()
        .with_chrome_profiles_dir(profiles_dir.path())
        .run(
            &IntegrationContext::default(),
            TestSuite::parallel(Parallelism::parallel(2))
                .with(page_title_test("page title one"))
                .with(page_title_test("page title two")),
        )
        .await?;

    assert_that!(dir_entry_count(profiles_dir.path())).is_equal_to(0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn capability_setup_setting_user_data_dir_fails_the_session() {
    let started = Arc::new(AtomicUsize::new(0));
    let err = runner()
        .with_chrome_capabilities(|caps| caps.add_arg("--user-data-dir=/custom"))
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(page_title_test_with_counter(
                "page title",
                Arc::clone(&started),
            )),
        )
        .await
        .expect_err("a configured user data dir should be rejected");

    assert_that!(started.load(Ordering::SeqCst)).is_equal_to(0);
    assert_that!(format!("{err:?}")).contains("`--user-data-dir` must not be set");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn unusable_chrome_profiles_dir_fails_the_run() {
    use std::os::unix::fs::PermissionsExt as _;

    let profiles_dir = ScratchDir::new("shared-profiles");
    fs::create_dir_all(profiles_dir.path()).expect("profiles dir should be created");
    fs::set_permissions(profiles_dir.path(), fs::Permissions::from_mode(0o777))
        .expect("permissions should be set");

    let err = runner()
        .with_chrome_profiles_dir(profiles_dir.path())
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(page_title_test("page title")),
        )
        .await
        .expect_err("a profiles dir other users can access should be rejected");

    assert_that!(err.to_string()).contains(BrowserTestError::CreateChromeProfiles.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_token_cancels_the_run_before_any_test_starts() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let started = Arc::new(AtomicUsize::new(0));

    let err = runner_with(Cancellation::from_token(cancellation))
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(page_title_test_with_counter(
                "not started",
                Arc::clone(&started),
            )),
        )
        .await
        .expect_err("a cancelled token should cancel the run");

    assert_that!(err.to_string()).contains(BrowserTestError::Cancelled.to_string());
    assert_that!(started.load(Ordering::SeqCst)).is_equal_to(0);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn cancellation_stops_the_run_and_its_browsers() {
    let profiles_dir = ScratchDir::new("cancellation-profiles");
    let cancellation = CancellationToken::new();
    let started = Arc::new(AtomicUsize::new(0));
    let (runner, outcomes) = record_outcomes(
        runner_with(Cancellation::from_token(cancellation.clone()))
            .with_chrome_profiles_dir(profiles_dir.path()),
    );

    let err = runner
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(RunForeverTest::new(move || cancellation.cancel()))
                .with(page_title_test_with_counter(
                    "not started",
                    Arc::clone(&started),
                )),
        )
        .await
        .expect_err("cancelling should stop the run");

    assert_that!(err.to_string()).contains(BrowserTestError::Cancelled.to_string());
    assert_that!(started.load(Ordering::SeqCst)).is_equal_to(0);
    // The running test is recorded as cancelled, the one not started is not recorded.
    assert_that!(outcomes.lock().unwrap().clone())
        .is_equal_to(vec![("run forever".to_owned(), TestOutcome::Cancelled)]);
    assert_that!(dir_entry_count(profiles_dir.path())).is_equal_to(0);
    let own_pid = std::process::id();
    let leftovers = || {
        processes().into_iter().any(|process| {
            (process.parent == own_pid && process.command.contains("chromedriver"))
                || process
                    .command
                    .contains(&*profiles_dir.path().to_string_lossy())
        })
    };
    assert_that!(eventually(|| !leftovers())).is_true();
}

/// Opens the fixture page and freezes it: its main thread runs an endless chain of microtasks.
/// Every later `WebDriver` command of the session waits for it.
struct FreezingTest;

#[async_trait]
impl BrowserTest<IntegrationContext> for FreezingTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("freezing")
    }

    fn settings(&self) -> TestSettings {
        TestSettings::new().with_time_limit(Duration::from_secs(3))
    }

    async fn run(&self, driver: &WebDriver, context: &IntegrationContext) -> Result<(), Report> {
        driver
            .goto(context.page_url)
            .await
            .context(IntegrationTestError::OpenTestPage)?;
        driver
            .execute(
                "setTimeout(() => {
                     const spin = () => queueMicrotask(spin);
                     spin();
                 });",
                vec![],
            )
            .await
            .context(IntegrationTestError::FreezePage)?;
        // Waits for the frozen page.
        driver
            .title()
            .await
            .context(IntegrationTestError::ReadTitle)?;
        Ok(())
    }
}

/// The name and outcome of every test of a run.
type Outcomes = Arc<std::sync::Mutex<Vec<(String, TestOutcome)>>>;

/// `runner`, recording the outcomes of the tests of its last run.
fn record_outcomes(runner: BrowserTestRunner) -> (BrowserTestRunner, Outcomes) {
    let outcomes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reported = Arc::clone(&outcomes);
    let runner = runner.with_report_consumer(move |report: &BrowserTestRunReport| {
        *reported.lock().unwrap() = report
            .tests
            .iter()
            .map(|test| (test.name.clone(), test.outcome))
            .collect();
    });
    (runner, outcomes)
}

/// The runner runs only the tests its filter selects, and every test of unfiltered suites.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_runner_applies_its_filter() {
    let (runner, outcomes) =
        record_outcomes(runner().with_filter(TestFilter::new().with_name("^selected$")));
    runner
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(page_title_test("selected"))
                .with(page_title_test("not selected"))
                .with_nested(
                    TestSuite::sequential()
                        .unfiltered()
                        .with(page_title_test("whole run check")),
                ),
        )
        .await
        .unwrap();
    assert_that!(outcomes.lock().unwrap().clone()).contains_exactly([
        ("selected".to_owned(), TestOutcome::Passed),
        ("whole run check".to_owned(), TestOutcome::Passed),
    ]);
}

/// A filter selecting none of the tests fails the run, even if unfiltered suites have tests, so
/// that a mistyped filter doesn't pass it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_filter_selecting_no_test_fails_the_run() {
    let (runner, outcomes) =
        record_outcomes(runner().with_filter(TestFilter::new().with_tag("button,focus")));
    let err = runner
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(page_title_test("untagged"))
                .with_nested(
                    TestSuite::sequential()
                        .unfiltered()
                        .with(page_title_test("whole run check")),
                ),
        )
        .await
        .expect_err("the filter selects no test");
    assert_that!(err.current_context()).is_equal_to(&BrowserTestError::NoTestSelected {
        name_patterns: Vec::new(),
        tag_patterns: vec!["button,focus".to_owned()],
    });
    assert_that!(outcomes.lock().unwrap().clone()).is_empty();
}

/// A test exceeding its own time limit on a frozen page times out, the page is closed (so its
/// session quits), the report names it, and the next test runs.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_frozen_page_times_out_and_is_closed() {
    let (runner, outcomes) = record_outcomes(runner());
    let start = Instant::now();

    let err = runner
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(FreezingTest)
                .with(page_title_test("after a frozen page")),
        )
        .await
        .expect_err("the frozen test times out");

    let report = format!("{err:?}");
    assert_that!(&report)
        .contains("Browser test 'freezing' exceeded its time limit of 3.00s.")
        .contains("Frozen: its page didn't respond within 2s")
        .contains(FIXTURE_PAGE_URL);
    assert_that!(outcomes.lock().unwrap().clone()).is_equal_to(vec![
        ("freezing".to_owned(), TestOutcome::TimedOut),
        ("after a frozen page".to_owned(), TestOutcome::Passed),
    ]);
    // Without closing the page, quitting its session waits for it.
    assert_that!(start.elapsed()).is_less_than(Duration::from_secs(60));
}

/// A test without a time limit of its own exceeding the runner's on pages that respond times out,
/// and its report says so.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_test_waiting_forever_times_out() {
    let (runner, outcomes) = record_outcomes(runner().with_time_limit(Duration::from_secs(1)));

    let err = runner
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(RunForeverTest::new(|| {})),
        )
        .await
        .expect_err("the test times out");

    assert_that!(format!("{err:?}"))
        .contains("Browser test 'run forever' exceeded its time limit of 1.00s.")
        .contains("Its pages respond: none of them is frozen.");
    assert_that!(outcomes.lock().unwrap().clone())
        .is_equal_to(vec![("run forever".to_owned(), TestOutcome::TimedOut)]);
}

/// The page `serve_frame_host` serves at `/frame`.
const FRAME_PAGE: &str = "<!doctype html><title>frame</title>";

/// Serves a page at `/` whose frame comes from another site (`127.0.0.1` rather than `localhost`),
/// so that it runs in a process of its own, and the frame at `/frame`. Returns the page's URL.
async fn serve_frame_host() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding a local port should succeed");
    let port = listener
        .local_addr()
        .expect("listener should have an address")
        .port();
    let host_page = format!(
        "<!doctype html><title>frame host</title><iframe src=\"http://127.0.0.1:{port}/frame\"></iframe>"
    );
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let host_page = host_page.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let body = if request.starts_with("GET /frame ") {
                    FRAME_PAGE
                } else {
                    host_page.as_str()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://localhost:{port}/")
}

/// Opens `url`, whose frame comes from another site, and freezes the frame: its main thread runs
/// an endless chain of microtasks. Every later `WebDriver` command in the frame waits for it.
struct FreezingFrameTest {
    url: String,
}

#[async_trait]
impl BrowserTest<IntegrationContext> for FreezingFrameTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("freezing frame")
    }

    fn settings(&self) -> TestSettings {
        TestSettings::new().with_time_limit(Duration::from_secs(3))
    }

    async fn run(&self, driver: &WebDriver, _context: &IntegrationContext) -> Result<(), Report> {
        driver
            .goto(&self.url)
            .await
            .context(IntegrationTestError::OpenTestPage)?;
        driver
            .enter_frame(0)
            .await
            .context(IntegrationTestError::FreezePage)?;
        driver
            .execute(
                "setTimeout(() => {
                     const spin = () => queueMicrotask(spin);
                     spin();
                 });",
                vec![],
            )
            .await
            .context(IntegrationTestError::FreezePage)?;
        // Waits for the frozen frame.
        driver
            .execute("return document.title;", vec![])
            .await
            .context(IntegrationTestError::ReadTitle)?;
        Ok(())
    }
}

/// A frozen frame of another site (an out-of-process `iframe`) is found and closed as well, and
/// the next test runs.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_frozen_frame_of_another_site_is_closed() {
    let url = serve_frame_host().await;
    let (runner, outcomes) = record_outcomes(runner());
    let start = Instant::now();

    let err = runner
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(FreezingFrameTest { url })
                .with(page_title_test("after a frozen frame")),
        )
        .await
        .expect_err("the frozen test times out");

    assert_that!(format!("{err:?}"))
        .contains("Browser test 'freezing frame' exceeded its time limit of 3.00s.")
        .contains("Frozen: its page didn't respond within 2s")
        .contains("/frame (a frame)");
    assert_that!(outcomes.lock().unwrap().clone()).is_equal_to(vec![
        ("freezing frame".to_owned(), TestOutcome::TimedOut),
        ("after a frozen frame".to_owned(), TestOutcome::Passed),
    ]);
    // Without closing the frame, quitting its session waits for it until it is abandoned.
    assert_that!(start.elapsed()).is_less_than(Duration::from_secs(30));
}

/// Opens the fixture page, opens an `alert`, and waits for it to be closed, which never happens.
struct DialogTest;

#[async_trait]
impl BrowserTest<IntegrationContext> for DialogTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("dialog")
    }

    fn settings(&self) -> TestSettings {
        TestSettings::new().with_time_limit(Duration::from_secs(3))
    }

    async fn run(&self, driver: &WebDriver, context: &IntegrationContext) -> Result<(), Report> {
        driver
            .goto(context.page_url)
            .await
            .context(IntegrationTestError::OpenTestPage)?;
        driver
            .execute("setTimeout(() => alert('waiting for you'));", vec![])
            .await
            .context(IntegrationTestError::OpenDialog)?;
        std::future::pending().await
    }
}

/// A test timing out while a dialog is open says so, rather than calling its page frozen, and the
/// next test runs.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_dialog_is_named_rather_than_a_frozen_page() {
    let (runner, outcomes) = record_outcomes(runner());

    let err = runner
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential()
                .with(DialogTest)
                .with(page_title_test("after a dialog")),
        )
        .await
        .expect_err("the test times out");

    assert_that!(format!("{err:?}"))
        .contains("Browser test 'dialog' exceeded its time limit of 3.00s.")
        .contains("A dialog is open, which its page waits for: waiting for you")
        .does_not_contain("Frozen");
    assert_that!(outcomes.lock().unwrap().clone()).is_equal_to(vec![
        ("dialog".to_owned(), TestOutcome::TimedOut),
        ("after a dialog".to_owned(), TestOutcome::Passed),
    ]);
}

/// Run by `sigint_cancels_the_run` and `sigterm_cancels_the_run` in a child process. Does nothing
/// when run on its own.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "started by `sigint_cancels_the_run` and `sigterm_cancels_the_run`"]
async fn child_run_to_be_cancelled_by_a_signal() -> RunnerResult {
    child_run(runner_with(Cancellation::on_shutdown_signals())).await
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sigint_cancels_the_run() {
    // What a terminal does on Ctrl-C: interrupt the whole foreground process group.
    assert_signal_cancels_the_run("child_run_to_be_cancelled_by_a_signal", "INT").await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sigterm_cancels_the_run() {
    // What `kill`, `timeout`, and CI runners send.
    assert_signal_cancels_the_run("child_run_to_be_cancelled_by_a_signal", "TERM").await;
}

/// Run by `sigint_cancels_a_run_after_the_runtime_of_an_earlier_run_ended` in a child process.
/// Does nothing when run on its own.
#[cfg(unix)]
#[test]
#[ignore = "started by `sigint_cancels_a_run_after_the_runtime_of_an_earlier_run_ended`"]
fn child_run_on_a_second_runtime_to_be_cancelled_by_a_signal() -> RunnerResult {
    if std::env::var_os(CHILD_RUN_DIR_ENV).is_none() {
        return Ok(());
    }
    let runtime = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime should build")
    };
    // Starts listening for the shutdown signals, on a runtime that ends right after. Only runs
    // with tests do.
    runtime().block_on(runner_with(Cancellation::on_shutdown_signals()).run(
        &IntegrationContext::default(),
        TestSuite::sequential().with(page_title_test("page title")),
    ))?;
    runtime().block_on(child_run(runner_with(Cancellation::on_shutdown_signals())))
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sigint_cancels_a_run_after_the_runtime_of_an_earlier_run_ended() {
    // Each `#[tokio::test]` has a runtime of its own, while Tokio never removes its signal handlers.
    assert_signal_cancels_the_run(
        "child_run_on_a_second_runtime_to_be_cancelled_by_a_signal",
        "INT",
    )
    .await;
}

/// Send `signal` to the process group of the child run `child_test`, and check that the run was
/// cancelled and left nothing behind.
#[cfg(unix)]
async fn assert_signal_cancels_the_run(child_test: &str, signal: &str) {
    let mut child = ChildRun::start(child_test).await;
    let groups = process_groups_of_tree(child.id());

    child.signal_group(signal);
    let status = child.wait_for_exit();

    // The run returned its error, so the test failed instead of being killed by the signal.
    assert_that!(status.code()).is_equal_to(Some(101));
    // libtest prints the returned error with `Debug`, which prints a report's messages.
    assert_that!(child.output()).contains(BrowserTestError::Cancelled.to_string());
    assert_that!(eventually(|| !groups
        .iter()
        .any(|group| process_group_exists(*group))))
    .is_true();
    assert_that!(dir_entry_count(&child.profiles_dir())).is_equal_to(0);
}

/// Run by `next_run_removes_the_chrome_profiles_of_a_killed_run` in a child process. Does nothing
/// when run on its own.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "started by `next_run_removes_the_chrome_profiles_of_a_killed_run`"]
async fn child_run_to_be_killed() -> RunnerResult {
    child_run(runner()).await
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn next_run_removes_the_chrome_profiles_of_a_killed_run() -> RunnerResult {
    let mut child = ChildRun::start("child_run_to_be_killed").await;
    let profiles_dir = child.profiles_dir();

    child.kill();
    // The killed run could not clean up.
    assert_that!(dir_entry_count(&profiles_dir)).is_equal_to(1);

    runner()
        .with_chrome_profiles_dir(&profiles_dir)
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(page_title_test("page title")),
        )
        .await?;

    assert_that!(dir_entry_count(&profiles_dir)).is_equal_to(0);
    Ok(())
}

/// Run `runner` the way a [`ChildRun`] expects: with one test that runs until stopped, keeping
/// profiles in the child run's directory. Does nothing outside a child run.
#[cfg(unix)]
async fn child_run(runner: BrowserTestRunner) -> RunnerResult {
    let Some(dir) = std::env::var_os(CHILD_RUN_DIR_ENV).map(PathBuf::from) else {
        return Ok(());
    };
    let ready_file = ChildRun::ready_file(&dir);
    runner
        .with_chrome_profiles_dir(ChildRun::profiles_dir_in(&dir))
        .run(
            &IntegrationContext::default(),
            TestSuite::sequential().with(RunForeverTest::new(move || {
                fs::write(&ready_file, "").expect("ready file should be writable");
            })),
        )
        .await
}

/// An ignored test of this binary, run in a child process in a process group of its own, so that
/// signals sent to that group do not reach the test harness. Its process tree is killed with
/// `SIGKILL` when dropped, even if the test fails.
#[cfg(unix)]
struct ChildRun {
    dir: ScratchDir,
    process: std::process::Child,
}

#[cfg(unix)]
impl ChildRun {
    /// Start the ignored test `test_name`, and wait until its browser test runs.
    async fn start(test_name: &str) -> Self {
        use std::{
            os::unix::process::CommandExt as _,
            time::{Duration, Instant},
        };

        let dir = ScratchDir::new(test_name);
        fs::create_dir_all(dir.path()).expect("scratch dir should be created");
        let output = fs::File::create(dir.path().join("output")).expect("output file");
        let process = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([test_name, "--exact", "--ignored"])
            .env(CHILD_RUN_DIR_ENV, dir.path())
            .process_group(0)
            .stdout(output)
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("child run should start");
        let mut child = Self { dir, process };

        let deadline = Instant::now() + Duration::from_secs(120);
        while !Self::ready_file(child.dir.path()).exists() {
            if let Some(status) = child.process.try_wait().expect("child should be waitable") {
                panic!("child run exited before its test ran: {status}");
            }
            assert_that!(Instant::now()).is_less_than(deadline);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        child
    }

    fn ready_file(dir: &Path) -> PathBuf {
        dir.join("ready")
    }

    fn profiles_dir_in(dir: &Path) -> PathBuf {
        dir.join("profiles")
    }

    fn profiles_dir(&self) -> PathBuf {
        Self::profiles_dir_in(self.dir.path())
    }

    fn id(&self) -> u32 {
        self.process.id()
    }

    /// The child's standard output.
    fn output(&self) -> String {
        fs::read_to_string(self.dir.path().join("output")).expect("output should be readable")
    }

    /// Send `signal`, e.g. `INT`, to the child's process group.
    fn signal_group(&self, signal: &str) {
        let status = std::process::Command::new("kill")
            .args([&format!("-{signal}"), "--", &format!("-{}", self.id())])
            .status()
            .expect("kill should run");
        assert_that!(status.success()).is_true();
    }

    /// Wait until the child exited, at most a minute.
    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.process.try_wait().expect("child should be waitable") {
                return status;
            }
            assert_that!(Instant::now()).is_less_than(deadline);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Kill the child and its descendants with `SIGKILL`, and wait until they all exited.
    ///
    /// `tokio-process-tools` starts `ChromeDriver` in a process group of its own, which its Chrome
    /// processes join. Killing the child's group alone would leave them running, so every group in
    /// the child's process tree is killed.
    fn kill(&mut self) {
        let groups = process_groups_of_tree(self.id());
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .arg("--")
            .args(groups.iter().map(|group| format!("-{group}")))
            .status();
        let _ = self.process.wait();
        // Dying Chrome processes must not write into profiles that the next run removes.
        assert_that!(eventually(|| !groups
            .iter()
            .any(|group| process_group_exists(*group))))
        .is_true();
    }
}

#[cfg(unix)]
impl Drop for ChildRun {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A process as listed by `ps`.
#[cfg(unix)]
struct Process {
    id: u32,
    parent: u32,
    group: u32,
    command: String,
}

/// All processes of the system.
#[cfg(unix)]
fn processes() -> Vec<Process> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,args="])
        .output()
        .expect("ps should list processes");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let mut number = || fields.next()?.parse::<u32>().ok();
            let (id, parent, group) = (number()?, number()?, number()?);
            let command = fields.collect::<Vec<_>>().join(" ");
            Some(Process {
                id,
                parent,
                group,
                command,
            })
        })
        .collect()
}

/// The process groups of `root` and all its descendants.
#[cfg(unix)]
fn process_groups_of_tree(root: u32) -> std::collections::BTreeSet<u32> {
    let processes = processes();
    let mut tree = vec![root];
    let mut next = 0;
    while let Some(&parent) = tree.get(next) {
        tree.extend(
            processes
                .iter()
                .filter(|process| process.parent == parent)
                .map(|process| process.id),
        );
        next += 1;
    }
    processes
        .iter()
        .filter(|process| tree.contains(&process.id))
        .map(|process| process.group)
        .collect()
}

/// Whether any process is left in the process group `group`.
#[cfg(unix)]
fn process_group_exists(group: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", "--", &format!("-{group}")])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether `condition` holds within ten seconds. Processes take a moment to exit.
#[cfg(unix)]
fn eventually(condition: impl Fn() -> bool) -> bool {
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

fn dir_entry_count(dir: &Path) -> usize {
    fs::read_dir(dir).expect("dir should be readable").count()
}

/// A directory below `target/` for one test, removed when dropped, even if the test fails.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = fs::remove_dir_all(&path);
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn page_title_test(name: impl Into<String>) -> PageTitleTest {
    PageTitleTest {
        name: name.into(),
        started: None,
    }
}

fn page_title_test_with_counter(
    name: impl Into<String>,
    started: Arc<AtomicUsize>,
) -> PageTitleTest {
    PageTitleTest {
        name: name.into(),
        started: Some(started),
    }
}

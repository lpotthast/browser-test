//! Integration tests for public browser runner behavior.

use std::{
    borrow::Cow,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use assertr::prelude::*;
use browser_test::thirtyfour::WebDriver;
use browser_test::{
    BrowserTest, BrowserTestError, BrowserTestFailurePolicy, BrowserTestParallelism,
    BrowserTestRunner, BrowserTests, BrowserTimeouts, ElementQueryWaitConfig, async_trait,
};
use rootcause::Report;
use rootcause::prelude::ResultExt;
use serial_test::serial;

type RunnerResult = Result<(), Report<BrowserTestError>>;

const FIXTURE_PAGE_URL: &str =
    "data:text/html,%3C!doctype%20html%3E%3Ctitle%3Ebrowser-test%20fixture%3C/title%3E";
const FIXTURE_PAGE_TITLE: &str = "browser-test fixture";

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
}

struct PageTitleTest {
    name: String,
    started: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl BrowserTest<IntegrationContext, IntegrationTestError> for PageTitleTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.name.as_str())
    }

    async fn run(
        &self,
        driver: &WebDriver,
        context: &IntegrationContext,
    ) -> Result<(), Report<IntegrationTestError>> {
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
            return Err(Report::new(IntegrationTestError::UnexpectedTitle {
                expected: context.expected_title,
                actual: title,
            }));
        }

        Ok(())
    }
}

struct IntentionalFailureTest {
    started: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl BrowserTest<IntegrationContext, IntegrationTestError> for IntentionalFailureTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("intentional failure")
    }

    async fn run(
        &self,
        _driver: &WebDriver,
        _context: &IntegrationContext,
    ) -> Result<(), Report<IntegrationTestError>> {
        if let Some(started) = &self.started {
            started.fetch_add(1, Ordering::SeqCst);
        }

        Err(Report::new(IntegrationTestError::IntentionalFailure))
    }
}

struct PanicTest {
    started: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl BrowserTest<IntegrationContext, IntegrationTestError> for PanicTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("intentional panic")
    }

    async fn run(
        &self,
        _driver: &WebDriver,
        _context: &IntegrationContext,
    ) -> Result<(), Report<IntegrationTestError>> {
        if let Some(started) = &self.started {
            started.fetch_add(1, Ordering::SeqCst);
        }

        panic!("intentional browser test panic");
    }
}

#[derive(Clone, Copy)]
enum MetadataPanicHook {
    Name,
    WebdriverTimeouts,
    ElementQueryWait,
}

struct MetadataPanicTest {
    panic_in: MetadataPanicHook,
}

#[async_trait]
impl BrowserTest<IntegrationContext, IntegrationTestError> for MetadataPanicTest {
    fn name(&self) -> Cow<'_, str> {
        if matches!(self.panic_in, MetadataPanicHook::Name) {
            panic!("name hook failed");
        }

        Cow::Borrowed("metadata panic")
    }

    fn timeouts(&self) -> Option<BrowserTimeouts> {
        if matches!(self.panic_in, MetadataPanicHook::WebdriverTimeouts) {
            panic!("webdriver timeout hook failed");
        }

        None
    }

    fn element_query_wait(&self) -> Option<ElementQueryWaitConfig> {
        if matches!(self.panic_in, MetadataPanicHook::ElementQueryWait) {
            panic!("element query wait hook failed");
        }

        None
    }

    async fn run(
        &self,
        _driver: &WebDriver,
        _context: &IntegrationContext,
    ) -> Result<(), Report<IntegrationTestError>> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn default_sequential_fail_fast_runs_page_title_test() -> RunnerResult {
    BrowserTestRunner::new()
        .run(
            &IntegrationContext::default(),
            BrowserTests::new().with(page_title_test("page title")),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn explicit_sequential_runs_page_title_test() -> RunnerResult {
    BrowserTestRunner::new()
        .with_test_parallelism(BrowserTestParallelism::Sequential)
        .run(
            &IntegrationContext::default(),
            BrowserTests::new().with(page_title_test("page title")),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn bounded_parallel_runs_page_title_tests() -> RunnerResult {
    BrowserTestRunner::new()
        .with_test_parallelism(BrowserTestParallelism::Parallel(
            NonZeroUsize::new(2).expect("literal parallelism should be non-zero"),
        ))
        .run(
            &IntegrationContext::default(),
            BrowserTests::new()
                .with(page_title_test(String::from("page title one")))
                .with(page_title_test("page title two")),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_all_runs_successful_page_title_tests() -> RunnerResult {
    BrowserTestRunner::new()
        .with_failure_policy(BrowserTestFailurePolicy::RunAll)
        .run(
            &IntegrationContext::default(),
            BrowserTests::new()
                .with(page_title_test("page title one"))
                .with(page_title_test("page title two")),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_all_reports_intentional_failure_and_runs_page_title_test() {
    let err = BrowserTestRunner::new()
        .with_failure_policy(BrowserTestFailurePolicy::RunAll)
        .run(
            &IntegrationContext::default(),
            BrowserTests::new()
                .with(IntentionalFailureTest { started: None })
                .with(page_title_test("page title")),
        )
        .await
        .expect_err("run-all should report the intentional failure");

    assert_that!(err.to_string())
        .contains(BrowserTestError::RunTests { failed_tests: 1 }.to_string());
    assert_that!(err.children().len()).is_equal_to(1);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_all_reports_metadata_hook_panics_and_runs_remaining_page_title_test() {
    let started = Arc::new(AtomicUsize::new(0));
    let err = BrowserTestRunner::new()
        .with_failure_policy(BrowserTestFailurePolicy::RunAll)
        .run(
            &IntegrationContext::default(),
            BrowserTests::new()
                .with(MetadataPanicTest {
                    panic_in: MetadataPanicHook::Name,
                })
                .with(MetadataPanicTest {
                    panic_in: MetadataPanicHook::WebdriverTimeouts,
                })
                .with(MetadataPanicTest {
                    panic_in: MetadataPanicHook::ElementQueryWait,
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
    assert_that!(err.children().len()).is_equal_to(3);
    assert_that!(format!("{err:?}")).contains("unnamed test at index 0");
    assert_that!(format!("{err:?}")).contains("webdriver timeout hook failed");
    assert_that!(format!("{err:?}")).contains("element query wait hook failed");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn parallel_run_all_reports_panic_and_runs_remaining_page_title_tests() {
    let started = Arc::new(AtomicUsize::new(0));
    let err = BrowserTestRunner::new()
        .with_test_parallelism(BrowserTestParallelism::Parallel(
            NonZeroUsize::new(2).expect("literal parallelism should be non-zero"),
        ))
        .with_failure_policy(BrowserTestFailurePolicy::RunAll)
        .run(
            &IntegrationContext::default(),
            BrowserTests::new()
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
    assert_that!(err.children().len()).is_equal_to(1);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn parallel_fail_fast_waits_for_running_page_title_test_without_starting_more() {
    let started = Arc::new(AtomicUsize::new(0));
    let err = BrowserTestRunner::new()
        .with_test_parallelism(BrowserTestParallelism::Parallel(
            NonZeroUsize::new(2).expect("literal parallelism should be non-zero"),
        ))
        .with_failure_policy(BrowserTestFailurePolicy::FailFast)
        .run(
            &IntegrationContext::default(),
            BrowserTests::new()
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
    assert_that!(err.children().len()).is_equal_to(1);
    assert_that!(started.load(Ordering::SeqCst)).is_equal_to(2);
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

//! Integration tests for session reuse, session resets, and the run report.

use std::{
    borrow::Cow,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use assertr::prelude::*;
use browser_test::thirtyfour::{By, ChromiumLikeCapabilities, Key, WebDriver};
use browser_test::{
    BrowserTest, BrowserTestFailurePolicy, BrowserTestParallelism, BrowserTestRunOutcome,
    BrowserTestRunner, BrowserTests, BrowserTimeouts, RunSummary, SessionAcquisition,
    SessionRequirement, SessionReset, TestOutcome, async_trait,
};
use rootcause::{Report, report};
use serial_test::serial;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

const PAGE: &str =
    "<!doctype html><title>session reuse fixture</title><input id=\"input\" autofocus>";

/// Serves [`PAGE`] for every request on a random local port. Returns the base URL.
async fn serve_fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding a local port should succeed");
    let addr = listener
        .local_addr()
        .expect("listener should have an address");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{PAGE}",
                    PAGE.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

fn runner() -> BrowserTestRunner {
    BrowserTestRunner::new()
        .with_chrome_capabilities(|caps| {
            caps.add_arg("--no-sandbox")?;
            caps.add_arg("--disable-dev-shm-usage")?;
            Ok(())
        })
        .with_failure_policy(BrowserTestFailurePolicy::RunAll)
        .with_run_summary(RunSummary::Log)
}

/// Opens the fixture page.
struct Visit {
    name: &'static str,
    session: SessionRequirement,
    timeouts: Option<BrowserTimeouts>,
    fail: bool,
}

impl Visit {
    fn shared(name: &'static str) -> Self {
        Self {
            name,
            session: SessionRequirement::Shared,
            timeouts: None,
            fail: false,
        }
    }
}

#[async_trait]
impl BrowserTest<str> for Visit {
    fn name(&self) -> Cow<'_, str> {
        self.name.into()
    }

    fn timeouts(&self) -> Option<BrowserTimeouts> {
        self.timeouts
    }

    fn session(&self) -> SessionRequirement {
        self.session
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        browser_test::step("goto", base_url, driver.goto(base_url)).await?;
        if self.fail {
            return Err(report!("intentional failure"));
        }
        Ok(())
    }
}

fn sessions(outcome: &BrowserTestRunOutcome) -> Vec<&'static str> {
    outcome
        .report
        .tests
        .iter()
        .map(|test| match test.session {
            SessionAcquisition::Created { .. } => "created",
            SessionAcquisition::Reused { .. } => "reused",
            SessionAcquisition::None => "none",
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn shared_tests_reuse_one_session() {
    let base_url = serve_fixture().await;

    let outcome = runner()
        .run_with_report(
            base_url.as_str(),
            BrowserTests::new()
                .with(Visit::shared("one"))
                .with(Visit::shared("two"))
                .with(Visit::shared("three")),
        )
        .await;

    assert_that!(outcome.result.is_ok()).is_true();
    assert_that!(sessions(&outcome)).is_equal_to(vec!["created", "reused", "reused"]);
    let report = &outcome.report;
    assert_that!(report.sessions_created()).is_equal_to(1);
    assert_that!(report.sessions_reused()).is_equal_to(2);
    // Only the last test of the session quits it.
    let teardowns: Vec<bool> = report
        .tests
        .iter()
        .map(|test| test.teardown.is_some())
        .collect();
    assert_that!(teardowns).is_equal_to(vec![false, false, true]);
    assert_that!(
        report
            .tests
            .iter()
            .all(|test| test.steps["goto"].count == 1)
    )
    .is_true();
    assert_that!(report.slowest_steps()[0].1.count).is_equal_to(3);
    assert_that!(report.total >= report.webdriver_startup + report.body_time()).is_true();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn fresh_tests_and_different_settings_get_their_own_sessions() {
    let base_url = serve_fixture().await;

    let outcome = runner()
        .run_with_report(
            base_url.as_str(),
            BrowserTests::new()
                .with(Visit::shared("shared one"))
                .with(Visit {
                    session: SessionRequirement::Fresh,
                    ..Visit::shared("fresh")
                })
                .with(Visit::shared("shared two"))
                .with(Visit::shared("shared three"))
                .with(Visit {
                    timeouts: Some(
                        BrowserTimeouts::builder()
                            .script_timeout(Duration::from_secs(7))
                            .build(),
                    ),
                    ..Visit::shared("custom timeouts")
                }),
        )
        .await;

    assert_that!(outcome.result.is_ok()).is_true();
    assert_that!(sessions(&outcome))
        .is_equal_to(vec!["created", "created", "created", "reused", "created"]);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn failed_test_discards_its_session() {
    let base_url = serve_fixture().await;

    let outcome = runner()
        .run_with_report(
            base_url.as_str(),
            BrowserTests::new()
                .with(Visit::shared("passes"))
                .with(Visit {
                    fail: true,
                    ..Visit::shared("fails")
                })
                .with(Visit::shared("passes after failure")),
        )
        .await;

    assert_that!(outcome.result.is_err()).is_true();
    assert_that!(sessions(&outcome)).is_equal_to(vec!["created", "reused", "created"]);
    let outcomes: Vec<TestOutcome> = outcome
        .report
        .tests
        .iter()
        .map(|test| test.outcome)
        .collect();
    assert_that!(outcomes).is_equal_to(vec![
        TestOutcome::Passed,
        TestOutcome::Failed,
        TestOutcome::Passed,
    ]);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn disabled_reuse_creates_a_session_per_test() {
    let base_url = serve_fixture().await;

    let outcome = runner()
        .with_session_reuse(false)
        .run_with_report(
            base_url.as_str(),
            BrowserTests::new()
                .with(Visit::shared("one"))
                .with(Visit::shared("two")),
        )
        .await;

    assert_that!(outcome.result.is_ok()).is_true();
    assert_that!(sessions(&outcome)).is_equal_to(vec!["created", "created"]);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn parallel_slots_reuse_their_own_sessions() {
    let base_url = serve_fixture().await;

    let outcome = runner()
        .with_test_parallelism(BrowserTestParallelism::Parallel(
            NonZeroUsize::new(2).expect("literal parallelism should be non-zero"),
        ))
        .run_with_report(
            base_url.as_str(),
            BrowserTests::new()
                .with(Visit::shared("one"))
                .with(Visit::shared("two"))
                .with(Visit::shared("three"))
                .with(Visit::shared("four")),
        )
        .await;

    assert_that!(outcome.result.is_ok()).is_true();
    let report = &outcome.report;
    assert_that!(report.tests.len()).is_equal_to(4);
    assert_that!(report.sessions_created()).is_equal_to(2);
    assert_that!(report.sessions_reused()).is_equal_to(2);
    let mut slots: Vec<usize> = report.tests.iter().map(|test| test.slot).collect();
    slots.sort_unstable();
    assert_that!(slots).is_equal_to(vec![0, 0, 1, 1]);
}

/// Leaves state behind: storage, a cookie, a pressed key, a second window, and a resized window.
struct LeaveStateBehind;

#[async_trait]
impl BrowserTest<str> for LeaveStateBehind {
    fn name(&self) -> Cow<'_, str> {
        "leave state behind".into()
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        driver.goto(base_url).await?;
        driver
            .execute(
                "localStorage.setItem('key', 'value'); sessionStorage.setItem('key', 'value'); \
                 document.cookie = 'key=value; max-age=3600';",
                vec![],
            )
            .await?;
        let rect = driver.get_window_rect().await?;
        driver
            .set_window_rect(
                rect.x,
                rect.y,
                u32::try_from(rect.width).unwrap_or(800) / 2 + 50,
                u32::try_from(rect.height).unwrap_or(600) / 2 + 50,
            )
            .await?;
        driver.action_chain().key_down(Key::Shift).perform().await?;
        driver.new_tab().await?;
        Ok(())
    }
}

/// Checks that none of [`LeaveStateBehind`]'s state is left.
struct ExpectCleanState {
    initial_rect: Arc<std::sync::Mutex<Option<(i64, i64)>>>,
}

#[async_trait]
impl BrowserTest<str> for ExpectCleanState {
    fn name(&self) -> Cow<'_, str> {
        "expect clean state".into()
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        assert_that!(driver.windows().await?.len()).is_equal_to(1);
        assert_that!(driver.current_url().await?.as_str()).is_equal_to("about:blank");
        let rect = driver.get_window_rect().await?;
        let initial = self
            .initial_rect
            .lock()
            .expect("lock should not be poisoned")
            .expect("the first test records the initial size");
        assert_that!((rect.width, rect.height)).is_equal_to(initial);

        driver.goto(base_url).await?;
        let state = driver
            .execute(
                "return [localStorage.getItem('key'), sessionStorage.getItem('key'), document.cookie];",
                vec![],
            )
            .await?;
        assert_that!(state.json().clone()).is_equal_to(serde_json::json!([null, null, ""]));

        // Shift is no longer pressed.
        let input = driver.find(By::Id("input")).await?;
        input.click().await?;
        driver.action_chain().send_keys("a").perform().await?;
        assert_that!(input.prop("value").await?).is_equal_to(Some("a".to_owned()));
        Ok(())
    }
}

/// Records the initial window size.
struct RecordWindowSize {
    initial_rect: Arc<std::sync::Mutex<Option<(i64, i64)>>>,
}

#[async_trait]
impl BrowserTest<str> for RecordWindowSize {
    fn name(&self) -> Cow<'_, str> {
        "record window size".into()
    }

    async fn run(&self, driver: &WebDriver, _base_url: &str) -> Result<(), Report> {
        let rect = driver.get_window_rect().await?;
        *self
            .initial_rect
            .lock()
            .expect("lock should not be poisoned") = Some((rect.width, rect.height));
        Ok(())
    }
}

/// Counts its calls.
struct CountingReset(Arc<AtomicUsize>);

#[async_trait]
impl SessionReset for CountingReset {
    async fn reset(&self, driver: &WebDriver) -> Result<(), Report> {
        // Runs on the new blank tab.
        assert_that!(driver.current_url().await?.as_str()).is_equal_to("about:blank");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn reset_clears_browser_state_and_runs_custom_resets() {
    let base_url = serve_fixture().await;
    let initial_rect = Arc::new(std::sync::Mutex::new(None));
    let resets = Arc::new(AtomicUsize::new(0));

    let outcome = runner()
        .with_session_reset(CountingReset(Arc::clone(&resets)))
        .run_with_report(
            base_url.as_str(),
            BrowserTests::new()
                .with(RecordWindowSize {
                    initial_rect: Arc::clone(&initial_rect),
                })
                .with(LeaveStateBehind)
                .with(ExpectCleanState {
                    initial_rect: Arc::clone(&initial_rect),
                }),
        )
        .await;

    assert_that!(outcome.result.is_ok())
        .with_detail_message(format!("{:?}", outcome.result))
        .is_true();
    assert_that!(sessions(&outcome)).is_equal_to(vec!["created", "reused", "reused"]);
    assert_that!(resets.load(Ordering::SeqCst)).is_equal_to(2);
}

/// A reset that always fails.
struct FailingReset;

#[async_trait]
impl SessionReset for FailingReset {
    async fn reset(&self, _driver: &WebDriver) -> Result<(), Report> {
        Err(report!("intentional reset failure"))
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn failed_reset_discards_the_session_without_failing_tests() {
    let base_url = serve_fixture().await;

    let outcome = runner()
        .with_session_reset(FailingReset)
        .run_with_report(
            base_url.as_str(),
            BrowserTests::new()
                .with(Visit::shared("one"))
                .with(Visit::shared("two")),
        )
        .await;

    assert_that!(outcome.result.is_ok()).is_true();
    assert_that!(sessions(&outcome)).is_equal_to(vec!["created", "created"]);
}

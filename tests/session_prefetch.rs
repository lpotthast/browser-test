//! Integration tests for the session pool (sessions created ahead of their tests, reused
//! sessions) and for the run report.

use std::{
    borrow::Cow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use assertr::prelude::*;
use browser_test::{
    BrowserTest, BrowserTestError, BrowserTestRunReport, BrowserTestRunner, BrowserTests,
    CachedData, Cancellation, ElementQueryWait, FailurePolicy, Parallelism, SessionPreparation,
    SessionReset, SessionReuse, SessionSettings, StepExt, TestOutcome, TracingSummary, async_trait,
    thirtyfour::{ChromiumLikeCapabilities, WebDriver},
};
use rootcause::{Report, report};
use serial_test::serial;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

const PAGE: &str = "<!doctype html><title>session prefetch fixture</title><p>fixture</p>";

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
    BrowserTestRunner::new(Cancellation::disabled())
        .with_chrome_capabilities(|caps| {
            caps.add_arg("--no-sandbox")?;
            caps.add_arg("--disable-dev-shm-usage")?;
            Ok(())
        })
        .with_failure_policy(FailurePolicy::RunAll)
        .with_report_consumer(TracingSummary)
}

/// Tracks which tests started, in which order, and how many ran at once.
#[derive(Default)]
struct Tracker {
    started: Mutex<Vec<usize>>,
    running: AtomicUsize,
    max_running: AtomicUsize,
}

/// Opens the fixture page and stays there for `body`.
struct Visit {
    index: usize,
    body: Duration,
    fail: bool,
    fresh_session: bool,
    /// Whether the test has an element query wait of its own, and so a session created for it.
    dedicated: bool,
    tracker: Arc<Tracker>,
}

impl Visit {
    fn new(index: usize, body: Duration, tracker: &Arc<Tracker>) -> Self {
        Self {
            index,
            body,
            fail: false,
            fresh_session: false,
            dedicated: false,
            tracker: Arc::clone(tracker),
        }
    }
}

#[async_trait]
impl BrowserTest<str> for Visit {
    fn name(&self) -> Cow<'_, str> {
        format!("visit {}", self.index).into()
    }

    fn session_settings(&self) -> SessionSettings {
        let settings = SessionSettings::new().with_fresh_session(self.fresh_session);
        if self.dedicated {
            settings.with_element_query_wait(ElementQueryWait::new(
                Duration::from_secs(1),
                Duration::from_millis(100),
            ))
        } else {
            settings
        }
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        let tracker = &self.tracker;
        tracker.started.lock().unwrap().push(self.index);
        let running = tracker.running.fetch_add(1, Ordering::SeqCst) + 1;
        tracker.max_running.fetch_max(running, Ordering::SeqCst);

        let result = async {
            driver.goto(base_url).step("goto").detail(base_url).await?;
            tokio::time::sleep(self.body).await;
            if self.fail {
                return Err(report!("intentional failure"));
            }
            Ok(())
        }
        .await;

        tracker.running.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

fn visits(count: usize, body: Duration, tracker: &Arc<Tracker>) -> BrowserTests<str> {
    visits_in(BrowserTests::sequential(), count, body, tracker)
}

fn visits_in(
    group: BrowserTests<str>,
    count: usize,
    body: Duration,
    tracker: &Arc<Tracker>,
) -> BrowserTests<str> {
    (0..count).fold(group, |tests, index| {
        tests.with(Visit::new(index, body, tracker))
    })
}

/// The result and report of one run.
struct Run {
    result: Result<(), Report<BrowserTestError>>,
    report: BrowserTestRunReport,
}

/// Run `tests`, capturing the run's report through a report consumer.
async fn run(runner: BrowserTestRunner, base_url: &str, tests: BrowserTests<str>) -> Run {
    let captured = Arc::new(Mutex::new(None));
    let consumer_capture = Arc::clone(&captured);
    let result = runner
        .with_report_consumer(move |report: &BrowserTestRunReport| {
            *consumer_capture.lock().unwrap() = Some(report.clone());
        })
        .run(base_url, tests)
        .await;
    let report = captured
        .lock()
        .unwrap()
        .take()
        .expect("runs with tests hand their report to consumers");
    Run { result, report }
}

/// `(creation, wait)` of every recorded test.
fn session_timings(outcome: &Run) -> Vec<(Duration, Duration)> {
    outcome
        .report
        .tests
        .iter()
        .map(|test| {
            let session = test.session.expect("every test should have had a session");
            (session.preparation.duration(), session.wait)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn later_tests_find_their_session_ready() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());
    let consumed = Arc::new(Mutex::new(Vec::new()));
    let consumer = |name: &'static str| {
        let consumed = Arc::clone(&consumed);
        move |report: &BrowserTestRunReport| {
            consumed.lock().unwrap().push((name, report.tests.len()));
        }
    };

    let outcome = run(
        runner()
            .with_report_consumer(consumer("first"))
            .with_report_consumer(consumer("second")),
        base_url.as_str(),
        visits(3, Duration::from_secs(2), &tracker),
    )
    .await;

    assert_that!(outcome.result.is_ok()).is_true();
    assert_that!(consumed.lock().unwrap().clone()).is_equal_to(vec![("first", 3), ("second", 3)]);
    assert_that!(outcome.report.sessions_created()).is_equal_to(3);
    for (creation, wait) in &session_timings(&outcome)[1..] {
        // Created while the previous test ran.
        assert_that!(*wait < *creation / 2)
            .with_detail_message(format!(
                "waited {wait:?} for a session created in {creation:?}"
            ))
            .is_true();
    }
    assert_that!(
        outcome
            .report
            .tests
            .iter()
            .all(|test| test.teardown.is_some() && test.steps["goto"].count == 1)
    )
    .is_true();
    assert_that!(outcome.report.session_wait_time() < outcome.report.session_creation_time())
        .is_true();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn without_spare_sessions_tests_wait_for_their_session() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());

    let outcome = run(
        runner().with_spare_sessions(0),
        base_url.as_str(),
        visits(2, Duration::ZERO, &tracker),
    )
    .await;

    assert_that!(outcome.result.is_ok()).is_true();
    for (creation, wait) in session_timings(&outcome) {
        assert_that!(wait >= creation / 2)
            .with_detail_message(format!(
                "waited {wait:?} for a session created in {creation:?}"
            ))
            .is_true();
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn parallel_tests_start_in_order_on_bounded_slots() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());

    let outcome = run(
        runner(),
        base_url.as_str(),
        visits_in(
            BrowserTests::parallel(Parallelism::parallel(2)),
            5,
            Duration::from_millis(500),
            &tracker,
        ),
    )
    .await;

    assert_that!(outcome.result.is_ok()).is_true();
    // Tests take slots in order, but a test whose session is ready may start before an earlier
    // test whose browser is still starting. So only the set of started tests is fixed.
    let mut started = tracker.started.lock().unwrap().clone();
    started.sort_unstable();
    assert_that!(started).is_equal_to(vec![0, 1, 2, 3, 4]);
    assert_that!(tracker.max_running.load(Ordering::SeqCst)).is_equal_to(2);
    assert_that!(outcome.report.sessions_created()).is_equal_to(5);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn fail_fast_does_not_start_tests_whose_session_is_ready() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());
    let mut failing = Visit::new(0, Duration::from_secs(1), &tracker);
    failing.fail = true;

    let outcome = run(
        runner().with_failure_policy(FailurePolicy::FailFast),
        base_url.as_str(),
        BrowserTests::sequential()
            .with(failing)
            .with(Visit::new(1, Duration::ZERO, &tracker))
            .with(Visit::new(2, Duration::ZERO, &tracker)),
    )
    .await;

    assert_that!(outcome.result.is_err()).is_true();
    assert_that!(tracker.started.lock().unwrap().clone()).is_equal_to(vec![0]);
    assert_that!(outcome.report.tests.len()).is_equal_to(1);
    assert_that!(outcome.report.tests[0].outcome).is_equal_to(TestOutcome::Failed);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn fail_fast_does_not_start_tests_waiting_for_their_session() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());
    let mut failing = Visit::new(1, Duration::ZERO, &tracker);
    failing.fail = true;
    // Its session is created when its turn comes, which takes longer than the failing test.
    let mut waiting = Visit::new(2, Duration::ZERO, &tracker);
    waiting.dedicated = true;

    let outcome = run(
        runner().with_failure_policy(FailurePolicy::FailFast),
        base_url.as_str(),
        BrowserTests::sequential()
            // While it runs, a spare session gets ready for the failing test.
            .with(Visit::new(0, Duration::from_secs(1), &tracker))
            .with_nested(
                BrowserTests::parallel(Parallelism::parallel(2))
                    .with(failing)
                    .with(waiting),
            ),
    )
    .await;

    assert_that!(outcome.result.is_err()).is_true();
    assert_that!(tracker.started.lock().unwrap().clone()).is_equal_to(vec![0, 1]);
    assert_that!(outcome.report.tests.len()).is_equal_to(2);
}

/// Stores a value in `localStorage` and a cookie, or checks that neither exists.
struct Storage {
    write: bool,
}

#[async_trait]
impl BrowserTest<str> for Storage {
    fn name(&self) -> Cow<'_, str> {
        if self.write {
            "write storage"
        } else {
            "read storage"
        }
        .into()
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        driver.goto(base_url).await?;
        if self.write {
            driver
                .execute(
                    "localStorage.setItem('leak', '1'); document.cookie = 'leak=1';",
                    Vec::new(),
                )
                .await?;
            return Ok(());
        }
        let leaked = driver
            .execute(
                "return localStorage.getItem('leak') !== null || document.cookie.includes('leak');",
                Vec::new(),
            )
            .await?;
        if leaked.convert::<bool>()? {
            return Err(report!("state of the previous test leaked into this one"));
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn every_test_gets_a_fresh_browser() {
    let base_url = serve_fixture().await;

    let result = runner()
        .run(
            base_url.as_str(),
            BrowserTests::sequential()
                .with(Storage { write: true })
                .with(Storage { write: false }),
        )
        .await;

    assert_that!(result.is_ok()).is_true();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn nested_groups_limit_what_runs_at_the_same_time() {
    let base_url = serve_fixture().await;
    let all = Arc::new(Tracker::default());
    let serial = Arc::new(Tracker::default());
    let body = Duration::from_millis(700);
    let tests = BrowserTests::parallel(Parallelism::parallel(3))
        .with(Visit::new(0, body, &all))
        .with(Visit::new(1, body, &all))
        .with_nested(
            BrowserTests::sequential()
                .named("serial")
                .with(Visit::new(2, body, &serial))
                .with(Visit::new(3, body, &serial)),
        );

    let outcome = run(runner(), base_url.as_str(), tests).await;

    assert_that!(outcome.result.is_ok()).is_true();
    assert_that!(all.max_running.load(Ordering::SeqCst)).is_equal_to(2);
    assert_that!(serial.max_running.load(Ordering::SeqCst)).is_equal_to(1);
    assert_that!(serial.started.lock().unwrap().clone()).is_equal_to(vec![2, 3]);
    let groups: Vec<Option<&str>> = outcome
        .report
        .tests
        .iter()
        .map(|test| test.group.as_deref())
        .collect();
    assert_that!(groups).is_equal_to(vec![None, None, Some("serial"), Some("serial")]);
    assert_that!(outcome.report.groups.len()).is_equal_to(1);
    assert_that!(outcome.report.groups[0].name.as_str()).is_equal_to("serial");
    // The serial group ran its two tests one after another, alongside the other two tests.
    assert_that!(outcome.report.groups[0].duration >= body * 2).is_true();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_all_runs_the_rest_of_a_sequential_group_after_a_failure() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());
    let mut failing = Visit::new(0, Duration::ZERO, &tracker);
    failing.fail = true;

    let outcome = run(
        runner(),
        base_url.as_str(),
        BrowserTests::sequential()
            .with(failing)
            .with(Visit::new(1, Duration::ZERO, &tracker)),
    )
    .await;

    assert_that!(outcome.result.is_err()).is_true();
    assert_that!(tracker.started.lock().unwrap().clone()).is_equal_to(vec![0, 1]);
    let outcomes: Vec<TestOutcome> = outcome
        .report
        .tests
        .iter()
        .map(|test| test.outcome)
        .collect();
    assert_that!(outcomes).is_equal_to(vec![TestOutcome::Failed, TestOutcome::Passed]);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn fail_fast_still_runs_run_always_groups() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());
    let mut failing = Visit::new(0, Duration::ZERO, &tracker);
    failing.fail = true;

    let outcome = run(
        runner().with_failure_policy(FailurePolicy::FailFast),
        base_url.as_str(),
        BrowserTests::sequential()
            .with(failing)
            .with(Visit::new(1, Duration::ZERO, &tracker))
            .with_nested(
                BrowserTests::sequential()
                    .named("after all")
                    .run_always()
                    .with(Visit::new(2, Duration::ZERO, &tracker)),
            ),
    )
    .await;

    assert_that!(outcome.result.is_err()).is_true();
    assert_that!(tracker.started.lock().unwrap().clone()).is_equal_to(vec![0, 2]);
}

/// How every test's session was prepared, `"created"` or `"reset"`.
fn preparations(outcome: &Run) -> Vec<&'static str> {
    outcome
        .report
        .tests
        .iter()
        .map(|test| {
            match test
                .session
                .expect("every test should have had a session")
                .preparation
            {
                SessionPreparation::Created(_) => "created",
                SessionPreparation::Reset(_) => "reset",
            }
        })
        .collect()
}

/// The window size and position a [`Dirty`] test found, for [`Clean`] to compare with.
type SharedRect = Arc<Mutex<Option<browser_test::thirtyfour::Rect>>>;

/// Leaves state behind that a session reset must remove: storage, a cookie, a held key, a resized
/// window, a granted permission, and, if `second_window`, a second window showing the app.
struct Dirty {
    initial_rect: SharedRect,
    second_window: bool,
}

#[async_trait]
impl BrowserTest<str> for Dirty {
    fn name(&self) -> Cow<'_, str> {
        "dirty".into()
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        driver.goto(base_url).await?;
        *self.initial_rect.lock().unwrap() = Some(driver.get_window_rect().await?);
        driver
            .execute(
                "localStorage.setItem('leak', '1'); sessionStorage.setItem('leak', '1');
                 document.cookie = 'leak=1';",
                Vec::new(),
            )
            .await?;
        // A file in the Origin Private File System.
        driver
            .execute_async(
                "const done = arguments[0];
                 navigator.storage.getDirectory()
                     .then(dir => dir.getFileHandle('leak', { create: true }))
                     .then(file => file.createWritable())
                     .then(writer => writer.write('1').then(() => writer.close()))
                     .then(done);",
                Vec::new(),
            )
            .await?;
        driver
            .cdp()
            .send_raw(
                "Browser.grantPermissions",
                serde_json::json!({
                    "origin": base_url,
                    "permissions": ["clipboardReadWrite"],
                }),
            )
            .await?;
        driver
            .cdp()
            .send_raw(
                "Emulation.setUserAgentOverride",
                serde_json::json!({"userAgent": "Dirty"}),
            )
            .await?;
        driver
            .action_chain()
            .key_down(browser_test::thirtyfour::Key::Shift)
            .perform()
            .await?;
        driver.set_window_rect(0, 0, 640, 480).await?;
        if self.second_window {
            let second = driver.new_window().await?;
            driver.switch_to_window(second).await?;
            driver.goto(base_url).await?;
        }
        Ok(())
    }
}

/// Checks that the session shows none of the state [`Dirty`] left behind.
struct Clean {
    initial_rect: SharedRect,
    /// The windows the reset leaves: the session's first tab, and the test's own tab with
    /// [`SessionReset::NewContext`].
    windows: usize,
}

#[async_trait]
impl BrowserTest<str> for Clean {
    fn name(&self) -> Cow<'_, str> {
        "clean".into()
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        assert_that!(driver.windows().await?.len()).is_equal_to(self.windows);
        // An empty page: `about:blank` in a new user context, a `data:` URL after a manual reset.
        let url = driver.current_url().await?.to_string();
        assert_that!(["about:blank", "data:text/html,"]).contains(url.as_str());
        assert_that!(Some(driver.get_window_rect().await?))
            .is_equal_to(self.initial_rect.lock().unwrap().clone());

        driver.goto(base_url).await?;
        let state = driver
            .execute(
                "return [
                     localStorage.getItem('leak'),
                     sessionStorage.getItem('leak'),
                     document.cookie,
                     document.hasFocus(),
                     navigator.userAgent.includes('Dirty'),
                     history.length,
                 ];",
                Vec::new(),
            )
            .await?
            .convert::<(Option<String>, Option<String>, String, bool, bool, u32)>()?;
        // The history: the blank page the reset left, and this one.
        assert_that!(state).is_equal_to((None, None, String::new(), true, false, 2));
        let permission = driver
            .execute_async(
                "navigator.permissions.query({ name: 'clipboard-read' })
                     .then(status => arguments[0](status.state));",
                Vec::new(),
            )
            .await?
            .convert::<String>()?;
        assert_that!(permission).is_equal_to("prompt".to_owned());
        let files = driver
            .execute_async(
                "const done = arguments[0];
                 navigator.storage.getDirectory()
                     .then(async dir => { const names = []; for await (const name of dir.keys()) names.push(name); return names; })
                     .then(done);",
                Vec::new(),
            )
            .await?
            .convert::<Vec<String>>()?;
        assert_that!(files).is_empty();

        // Shift is no longer held: typing gives lowercase text.
        driver
            .execute(
                "const input = document.createElement('input');
                 document.body.append(input);
                 input.focus();",
                Vec::new(),
            )
            .await?;
        driver.action_chain().send_keys("a").perform().await?;
        let typed = driver
            .execute("return document.querySelector('input').value;", Vec::new())
            .await?
            .convert::<String>()?;
        assert_that!(typed).is_equal_to("a".to_owned());
        Ok(())
    }
}

/// Runs [`Dirty`], then [`Clean`] in the reset session.
async fn assert_reset_keeps_no_state(reset: SessionReset, second_window: bool, windows: usize) {
    let base_url = serve_fixture().await;
    let initial_rect = SharedRect::default();

    let outcome = run(
        runner()
            .with_session_reuse(SessionReuse::enabled().with_reset(reset))
            .with_spare_sessions(0),
        base_url.as_str(),
        BrowserTests::sequential()
            .with(Dirty {
                initial_rect: Arc::clone(&initial_rect),
                second_window,
            })
            .with(Clean {
                initial_rect,
                windows,
            }),
    )
    .await;

    if let Err(error) = &outcome.result {
        panic!("the clean test should pass: {error:?}");
    }
    assert_that!(preparations(&outcome)).is_equal_to(vec!["created", "reset"]);
    assert_that!(outcome.report.sessions_created()).is_equal_to(1);
    assert_that!(outcome.report.session_resets()).is_equal_to(1);
    // The reused session's teardown is its reset, reported with the next test.
    assert_that!(outcome.report.tests[0].teardown).is_none();
    assert_that!(outcome.report.tests[1].teardown.is_some()).is_true();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn new_context_resets_keep_no_state_of_earlier_tests() {
    // The session's first tab, and the tab of the test's user context.
    assert_reset_keeps_no_state(SessionReset::NewContext, false, 2).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn manual_resets_keep_no_state_of_earlier_tests() {
    assert_reset_keeps_no_state(SessionReset::manual([CachedData::Http]), true, 1).await;
}

/// Writes `localStorage` of a `file:` page, or checks that it is empty.
struct FileStorage {
    url: String,
    write: bool,
}

#[async_trait]
impl BrowserTest<str> for FileStorage {
    fn name(&self) -> Cow<'_, str> {
        format!("file storage (write: {})", self.write).into()
    }

    async fn run(&self, driver: &WebDriver, _base_url: &str) -> Result<(), Report> {
        driver.goto(&self.url).await?;
        let stored = driver
            .execute(
                "const stored = localStorage.getItem('leak');
                 localStorage.setItem('leak', '1');
                 return stored;",
                Vec::new(),
            )
            .await?
            .convert::<Option<String>>()?;
        if !self.write {
            assert_that!(stored).is_none();
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn manual_resets_clear_the_storage_of_file_pages() {
    let dir =
        std::env::temp_dir().join(format!("browser-test-file-storage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir should be created");
    let page = dir.join("fixture.html");
    std::fs::write(&page, PAGE).expect("page should be written");
    let url = format!("file://{}", page.display());

    let outcome = run(
        runner()
            .with_session_reuse(SessionReuse::enabled().with_reset(SessionReset::manual([])))
            .with_spare_sessions(0),
        "",
        BrowserTests::sequential()
            .with(FileStorage {
                url: url.clone(),
                write: true,
            })
            .with(FileStorage { url, write: false }),
    )
    .await;
    let _ = std::fs::remove_dir_all(&dir);

    if let Err(error) = &outcome.result {
        panic!("the second test should find no storage: {error:?}");
    }
    assert_that!(preparations(&outcome)).is_equal_to(vec!["created", "reset"]);
}

/// Leaves a page load timeout behind that no navigation can meet.
struct ImpatientPageLoads;

#[async_trait]
impl BrowserTest<str> for ImpatientPageLoads {
    fn name(&self) -> Cow<'_, str> {
        "impatient page loads".into()
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        driver.goto(base_url).await?;
        driver
            .update_timeouts(browser_test::thirtyfour::TimeoutConfiguration::new(
                None,
                Some(Duration::ZERO),
                None,
            ))
            .await?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn resets_run_with_the_sessions_timeouts() {
    let base_url = serve_fixture().await;
    let outcome = run(
        runner()
            .with_session_reuse(
                SessionReuse::enabled().with_reset(SessionReset::manual([CachedData::Http])),
            )
            .with_spare_sessions(0),
        base_url.as_str(),
        BrowserTests::sequential()
            .with(ImpatientPageLoads)
            .with(Visit::new(0, Duration::ZERO, &Arc::default())),
    )
    .await;

    if let Err(error) = &outcome.result {
        panic!("every test should pass: {error:?}");
    }
    assert_that!(preparations(&outcome)).is_equal_to(vec!["created", "reset"]);
}

/// Opens a window in the browser's default context, or navigates the session's first tab there.
/// Removing the test's user context would leave the state of the default context behind.
struct UsesDefaultContext {
    new_window: bool,
}

#[async_trait]
impl BrowserTest<str> for UsesDefaultContext {
    fn name(&self) -> Cow<'_, str> {
        format!("uses the default context (new window: {})", self.new_window).into()
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        let window = if self.new_window {
            driver.new_window().await?
        } else {
            let current = driver.window().await?;
            let windows = driver.windows().await?;
            windows
                .into_iter()
                .find(|window| *window != current)
                .expect("the session's first tab is open")
        };
        driver.switch_to_window(window).await?;
        driver.goto(base_url).await?;
        driver
            .execute("localStorage.setItem('leak', '1');", Vec::new())
            .await?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn new_context_sessions_quit_after_tests_using_the_default_context() {
    let base_url = serve_fixture().await;
    let outcome = run(
        runner()
            .with_session_reuse(SessionReuse::enabled())
            .with_spare_sessions(0),
        base_url.as_str(),
        BrowserTests::sequential()
            .with(UsesDefaultContext { new_window: true })
            .with(UsesDefaultContext { new_window: false })
            .with(Visit::new(0, Duration::ZERO, &Arc::default())),
    )
    .await;

    if let Err(error) = &outcome.result {
        panic!("every test should pass: {error:?}");
    }
    assert_that!(preparations(&outcome)).is_equal_to(vec!["created", "created", "created"]);
    assert_that!(outcome.report.session_resets()).is_equal_to(0);
}

/// Serves a page loading `/cached.js`, which browsers may cache for an hour. Returns the base URL
/// and how often `/cached.js` was requested.
async fn serve_cache_fixture() -> (String, Arc<AtomicUsize>) {
    const PAGE: &str =
        "<!doctype html><title>cache fixture</title><script src=\"/cached.js\"></script>";
    const SCRIPT: &str = "window.__cached = true;";
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding a local port should succeed");
    let addr = listener
        .local_addr()
        .expect("listener should have an address");
    let script_requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&script_requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let response = if request.starts_with("GET /cached.js ") {
                    counter.fetch_add(1, Ordering::SeqCst);
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/javascript\r\ncache-control: max-age=3600\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{SCRIPT}",
                        SCRIPT.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{PAGE}",
                        PAGE.len()
                    )
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), script_requests)
}

/// Loads the cache fixture's page and waits for its script.
struct LoadsCachedScript(usize);

#[async_trait]
impl BrowserTest<str> for LoadsCachedScript {
    fn name(&self) -> Cow<'_, str> {
        format!("loads the cached script {}", self.0).into()
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        driver.goto(base_url).await?;
        let loaded = driver
            .execute("return window.__cached === true;", Vec::new())
            .await?
            .convert::<bool>()?;
        assert_that!(loaded).is_true();
        Ok(())
    }
}

/// How often two tests in one reused session request the cacheable script.
async fn script_requests(reset: SessionReset) -> usize {
    let (base_url, requests) = serve_cache_fixture().await;
    let outcome = run(
        runner()
            .with_session_reuse(SessionReuse::enabled().with_reset(reset))
            .with_spare_sessions(0),
        base_url.as_str(),
        BrowserTests::sequential()
            .with(LoadsCachedScript(1))
            .with(LoadsCachedScript(2)),
    )
    .await;
    assert_that!(outcome.result.is_ok()).is_true();
    assert_that!(preparations(&outcome)).is_equal_to(vec!["created", "reset"]);
    requests.load(Ordering::SeqCst)
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn only_manual_resets_keeping_it_keep_the_http_cache() {
    assert_that!(script_requests(SessionReset::manual([CachedData::Http])).await).is_equal_to(1);
    assert_that!(script_requests(SessionReset::manual([])).await).is_equal_to(2);
    assert_that!(script_requests(SessionReset::NewContext).await).is_equal_to(2);
}

/// Leaves a page and goes back: whether it was restored from the back/forward cache (its
/// script state survived) is reported in `restored`.
struct GoesBack {
    restored: Arc<Mutex<Option<bool>>>,
    /// Whether the test has an element query wait of its own, and so a session created for it.
    dedicated: bool,
}

#[async_trait]
impl BrowserTest<str> for GoesBack {
    fn name(&self) -> Cow<'_, str> {
        "goes back".into()
    }

    fn session_settings(&self) -> SessionSettings {
        if self.dedicated {
            SessionSettings::new().with_element_query_wait(ElementQueryWait::new(
                Duration::from_secs(1),
                Duration::from_millis(100),
            ))
        } else {
            SessionSettings::new()
        }
    }

    async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
        driver.goto(base_url).await?;
        driver
            .execute("window.__marker = true;", Vec::new())
            .await?;
        driver.goto(&format!("{base_url}/other")).await?;
        driver.back().await?;
        let restored = driver
            .execute("return window.__marker === true;", Vec::new())
            .await?
            .convert::<bool>()?;
        *self.restored.lock().unwrap() = Some(restored);
        Ok(())
    }
}

/// Whether a page is restored from the back/forward cache in a reused session, or in a
/// `dedicated` one created for a test with settings of its own.
async fn restored_from_back_forward_cache(reuse: SessionReuse, dedicated: bool) -> bool {
    let base_url = serve_fixture().await;
    let restored = Arc::new(Mutex::new(None));
    let outcome = run(
        runner().with_session_reuse(reuse).with_spare_sessions(0),
        base_url.as_str(),
        BrowserTests::sequential().with(GoesBack {
            restored: Arc::clone(&restored),
            dedicated,
        }),
    )
    .await;
    assert_that!(outcome.result.is_ok()).is_true();
    restored.lock().unwrap().expect("the test ran")
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn reused_sessions_have_no_back_forward_cache_unless_enabled() {
    for reset in [SessionReset::NewContext, SessionReset::manual([])] {
        // Sessions created for tests with settings of their own are set up alike.
        for dedicated in [false, true] {
            let reuse = SessionReuse::enabled().with_reset(reset);
            assert_that!(restored_from_back_forward_cache(reuse, dedicated).await).is_false();
            assert_that!(
                restored_from_back_forward_cache(reuse.with_back_forward_cache(true), dedicated)
                    .await
            )
            .is_true();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn reused_sessions_run_tests_while_more_tests_need_them() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());

    let outcome = run(
        runner().with_session_reuse(SessionReuse::enabled()),
        base_url.as_str(),
        visits_in(
            BrowserTests::parallel(Parallelism::parallel(2)),
            8,
            Duration::from_millis(300),
            &tracker,
        ),
    )
    .await;

    assert_that!(outcome.result.is_ok()).is_true();
    // Two running tests plus two spares at most; every further test reuses one of them.
    assert_that!(outcome.report.sessions_created()).is_less_or_equal_to(4);
    assert_that!(outcome.report.session_resets()).is_greater_or_equal_to(4);
    assert_that!(outcome.report.to_string().as_str()).contains(", reset ");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn fresh_session_tests_get_a_session_no_test_ran_in() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());
    let mut failing = Visit::new(1, Duration::ZERO, &tracker);
    failing.fail = true;
    let mut fresh = Visit::new(3, Duration::ZERO, &tracker);
    fresh.fresh_session = true;

    let outcome = run(
        runner()
            .with_session_reuse(SessionReuse::enabled())
            .with_spare_sessions(0),
        base_url.as_str(),
        BrowserTests::sequential()
            .with(Visit::new(0, Duration::ZERO, &tracker))
            .with(failing)
            .with(Visit::new(2, Duration::ZERO, &tracker))
            .with(fresh)
            .with(Visit::new(4, Duration::ZERO, &tracker)),
    )
    .await;

    assert_that!(outcome.result.is_err()).is_true();
    // One session runs 0, 1 (failing) and 2. 3 needs a fresh one, so the pool creates it. 4 takes
    // the reset session of 2; 3's session isn't needed anymore and quits.
    assert_that!(preparations(&outcome))
        .is_equal_to(vec!["created", "reset", "reset", "created", "reset"]);
    assert_that!(outcome.report.sessions_created()).is_equal_to(2);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sessions_run_at_most_their_maximum_of_tests() {
    let base_url = serve_fixture().await;
    let tracker = Arc::new(Tracker::default());

    let outcome = run(
        runner()
            .with_session_reuse(
                SessionReuse::enabled().with_max_tests_per_session(2.try_into().unwrap()),
            )
            .with_spare_sessions(0),
        base_url.as_str(),
        visits(3, Duration::ZERO, &tracker),
    )
    .await;

    assert_that!(outcome.result.is_ok()).is_true();
    assert_that!(preparations(&outcome)).is_equal_to(vec!["created", "reset", "created"]);
}

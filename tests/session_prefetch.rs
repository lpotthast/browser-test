//! Integration tests for sessions created ahead of their tests, and for the run report.

use std::{
    borrow::Cow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use assertr::prelude::*;
use browser_test::thirtyfour::{ChromiumLikeCapabilities, WebDriver};
use browser_test::{
    BrowserTest, BrowserTestError, BrowserTestRunReport, BrowserTestRunner, BrowserTests,
    FailurePolicy, Parallelism, StepExt, TestOutcome, TracingSummary, async_trait,
};
use rootcause::{Report, report};
use serial_test::serial;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

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
    BrowserTestRunner::new()
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
    tracker: Arc<Tracker>,
}

impl Visit {
    fn new(index: usize, body: Duration, tracker: &Arc<Tracker>) -> Self {
        Self {
            index,
            body,
            fail: false,
            tracker: Arc::clone(tracker),
        }
    }
}

#[async_trait]
impl BrowserTest<str> for Visit {
    fn name(&self) -> Cow<'_, str> {
        format!("visit {}", self.index).into()
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
            (session.creation, session.wait)
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
        .with_group(
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
            .with_group(
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

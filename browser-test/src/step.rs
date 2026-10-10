//! Timed steps inside browser tests.

use std::{
    collections::BTreeMap,
    fmt::{self, Display},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll, ready},
    time::{Duration, Instant},
};

use crate::{
    failure_report::{PanicDetails, RecentSteps, StepEvent, StepLog},
    report::{FormatDuration, StepStats},
};

/// Default for [`crate::ProgressWarnings`]'s slow step threshold, also used for steps outside of
/// a runner.
pub(crate) const DEFAULT_SLOW_STEP: Duration = Duration::from_secs(2);

tokio::task_local! {
    static CURRENT_TEST: Arc<StepRecorder>;
}

/// Collects the steps of the currently running test.
#[derive(Debug)]
pub(crate) struct StepRecorder {
    slow_step: Option<Duration>,
    steps: Mutex<BTreeMap<String, StepStats>>,
    /// When the test started, for [`StepEvent::started`].
    started: Instant,
    /// The last steps, for failure reports.
    log: StepLog,
    /// Where the test panicked, recorded by the panic hook.
    panic: Mutex<Option<PanicDetails>>,
}

impl StepRecorder {
    pub(crate) fn new(slow_step: Option<Duration>) -> Arc<Self> {
        Arc::new(Self {
            slow_step,
            steps: Mutex::new(BTreeMap::new()),
            started: Instant::now(),
            log: StepLog::default(),
            panic: Mutex::new(None),
        })
    }

    /// The test's last steps.
    pub(crate) fn recent_steps(&self) -> Option<RecentSteps> {
        self.log.recent()
    }

    /// Where the test panicked, if it did.
    pub(crate) fn take_panic(&self) -> Option<PanicDetails> {
        self.panic
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Run `future` (a test body) so that [`Step`]s inside it record into this recorder.
    pub(crate) async fn scope<F: Future>(self: &Arc<Self>, future: F) -> F::Output {
        CURRENT_TEST.scope(Arc::clone(self), future).await
    }

    pub(crate) fn take(&self) -> BTreeMap<String, StepStats> {
        std::mem::take(&mut *self.steps.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn record(&self, kind: &str, duration: Duration) {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(kind.to_owned())
            .or_default()
            .record(duration);
    }
}

/// Time futures as steps of a browser test, such as a navigation or a wait.
///
/// Implemented for every future. Import it to call [`StepExt::step`].
pub trait StepExt: Future + Sized {
    /// Time this future as a step of the given `kind`.
    ///
    /// Awaiting the returned [`Step`] awaits this future and returns its output. Every step is
    /// logged at `debug` level with its duration. A step taking longer than the runner's slow
    /// step threshold (see [`crate::ProgressWarnings`], 2 seconds by default) is logged at `warn`
    /// level. Inside a test run by [`crate::BrowserTestRunner`], the step's duration is also added
    /// to the test's [`crate::BrowserTestRecord::steps`] under `kind`, and the run summary lists
    /// the step kinds that took the most time.
    ///
    /// A step that doesn't finish, because it is dropped (e.g. by a timeout around it) or a panic
    /// unwinds through it, is recorded as unfinished when dropped, with the time until then.
    ///
    /// Use a small, fixed set of `kind`s (e.g. `"goto"`, `"wait_for_text"`), so that they
    /// aggregate across tests, and add specifics (URL, selector, expected text) with
    /// [`Step::detail`], which is only logged. Steps may nest. A nested step's time counts towards
    /// every enclosing kind as well.
    ///
    /// Instrument your page-object helpers with steps to see where your tests spend time.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use browser_test::thirtyfour::{WebDriver, error::WebDriverResult};
    /// use browser_test::StepExt;
    ///
    /// async fn goto(driver: &WebDriver, url: &str) -> WebDriverResult<()> {
    ///     driver.goto(url).step("goto").detail(url).await
    /// }
    /// ```
    fn step(self, kind: &'static str) -> Step<Self> {
        Step {
            future: Box::pin(self),
            kind,
            detail: None,
            start: None,
            finished: false,
        }
    }
}

impl<F: Future> StepExt for F {}

/// A future timed as one step of a browser test, created by [`StepExt::step`].
#[must_use = "a step does nothing unless awaited"]
pub struct Step<F> {
    future: Pin<Box<F>>,
    kind: &'static str,
    detail: Option<String>,
    start: Option<Instant>,
    /// Whether the future finished. A step dropped before, e.g. by a timeout or a panic, is
    /// recorded as unfinished when dropped.
    finished: bool,
}

impl<F> Step<F> {
    /// Add specifics, such as a URL or a selector, to the step's log line. Details are not
    /// aggregated.
    pub fn detail(mut self, detail: impl Display) -> Self {
        self.detail = Some(detail.to_string());
        self
    }
}

impl<F> fmt::Debug for Step<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Step")
            .field("kind", &self.kind)
            .field("detail", &self.detail)
            .finish_non_exhaustive()
    }
}

impl<F: Future> Future for Step<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.get_mut();
        let start = *this.start.get_or_insert_with(Instant::now);
        let output = ready!(this.future.as_mut().poll(cx));
        this.finished = true;
        record_step(this.kind, this.detail.as_deref(), start, true);
        Poll::Ready(output)
    }
}

impl<F> Drop for Step<F> {
    fn drop(&mut self) {
        if let Some(start) = self.start
            && !self.finished
        {
            record_step(self.kind, self.detail.as_deref(), start, false);
        }
    }
}

/// Whether this code runs in a test's body.
pub(crate) fn in_test() -> bool {
    CURRENT_TEST.try_with(|_| ()).is_ok()
}

/// Record where the running test panicked (called by the panic hook; outside a test, it does
/// nothing). The last panic of a test counts: a panic failing the test ends it, while one before
/// was caught by the test itself.
pub(crate) fn record_panic(details: PanicDetails) {
    let _ = CURRENT_TEST.try_with(|recorder| {
        *recorder
            .panic
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(details);
    });
}

/// Log a step that ended, `finished` or dropped before, and add it to the current test's record, if
/// any.
fn record_step(kind: &'static str, detail: Option<&str>, start: Instant, finished: bool) {
    let duration = start.elapsed();
    let recorder = CURRENT_TEST.try_with(Arc::clone).ok();
    let slow_step = match &recorder {
        Some(recorder) => recorder.slow_step,
        None => Some(DEFAULT_SLOW_STEP),
    };
    if let Some(recorder) = &recorder {
        recorder.record(kind, duration);
        recorder.log.push(StepEvent {
            kind,
            detail: detail.map(str::to_owned),
            started: start.saturating_duration_since(recorder.started),
            duration,
            finished,
        });
    }
    let detail = detail
        .map(|detail| format!(" {detail}"))
        .unwrap_or_default();
    let ended = if finished {
        "took"
    } else {
        "ended unfinished after"
    };
    if slow_step.is_some_and(|slow_step| duration > slow_step) {
        tracing::warn!(
            step = kind,
            duration_ms = duration.as_millis(),
            "Slow step: {kind}{detail} {ended} {}",
            FormatDuration(duration),
        );
    } else {
        tracing::debug!(
            step = kind,
            duration_ms = duration.as_millis(),
            "Step {kind}{detail} {ended} {}",
            FormatDuration(duration),
        );
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    #[test]
    fn steps_record_into_the_enclosing_test() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime should build");
        let first = StepRecorder::new(None);
        let second = StepRecorder::new(None);

        runtime.block_on(async {
            // Two tests polled concurrently on the same task record into their own recorders.
            futures_util::future::join(
                first.scope(async {
                    async {}.step("goto").detail("/a").await;
                    async {}.step("goto").await;
                }),
                second.scope(async {
                    async {}.step("wait").detail("#id").await;
                }),
            )
            .await;
            // Outside of a test, steps are only logged.
            async {}.step("goto").await;
        });

        let first = first.take();
        let second = second.take();
        assert_that!(first.keys().collect::<Vec<_>>()).is_equal_to(vec!["goto"]);
        assert_that!(first["goto"].count).is_equal_to(2);
        assert_that!(second.keys().collect::<Vec<_>>()).is_equal_to(vec!["wait"]);
        assert_that!(second["wait"].count).is_equal_to(1);
    }

    #[test]
    fn steps_that_do_not_finish_are_recorded_unfinished() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime should build");
        let recorder = StepRecorder::new(None);

        runtime.block_on(recorder.scope(async {
            // Cut off by a timeout.
            let pending = std::future::pending::<()>().step("wait");
            let _ = tokio::time::timeout(Duration::from_millis(1), pending).await;
            // Unwound by a panic.
            let panicked = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                async { panic!("assertion failed") }.step("assert"),
            ))
            .await;
            assert!(panicked.is_err());
            // Never polled.
            drop(async {}.step("unpolled"));
        }));

        let steps = recorder.take();
        assert_that!(steps.keys().collect::<Vec<_>>()).is_equal_to(vec!["assert", "wait"]);
        let recent = recorder
            .recent_steps()
            .expect("the steps are recent")
            .to_string();
        let lines: Vec<_> = recent.lines().skip(1).collect();
        assert_that!(lines.len()).is_equal_to(2);
        assert_that!(lines[0]).contains(" wait (");
        assert_that!(lines[1]).contains(" assert (");
        assert_that!(lines.iter().all(|line| line.ends_with(", unfinished)"))).is_true();
    }

    #[test]
    fn the_last_panic_of_a_test_counts() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime should build");
        let recorder = StepRecorder::new(None);
        let panic_at = |location: &str| PanicDetails {
            location: Some(location.to_owned()),
            frames: None,
        };

        runtime.block_on(recorder.scope(async {
            // One the test caught, then the one failing it.
            record_panic(panic_at("tests/ui.rs:10:5"));
            record_panic(panic_at("tests/ui.rs:20:5"));
        }));

        let panic = recorder.take_panic().expect("the test panicked");
        assert_that!(panic.location).is_equal_to(Some("tests/ui.rs:20:5".to_owned()));
    }

    #[test]
    fn step_returns_the_future_output() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime should build");

        let output = runtime.block_on(async { 42 }.step("compute"));

        assert_that!(output).is_equal_to(42);
    }
}

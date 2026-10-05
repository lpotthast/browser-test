//! Timed steps inside browser tests.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::report::{FormatDuration, StepStats};

/// Default for [`crate::ProgressWarnings`]'s slow step threshold, also used by [`step`] outside of
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
}

impl StepRecorder {
    pub(crate) fn new(slow_step: Option<Duration>) -> Arc<Self> {
        Arc::new(Self {
            slow_step,
            steps: Mutex::new(BTreeMap::new()),
        })
    }

    /// Run `future` (a test body) so that [`step`]s inside it record into this recorder.
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

/// Time one step of a browser test, such as a navigation or a wait.
///
/// Awaits `future` and returns its output. Every step is logged at `debug` level with its
/// duration; a step taking longer than the runner's slow step threshold (see
/// [`crate::ProgressWarnings`], 2 seconds by default) is logged at `warn` level. When called inside
/// a test run by [`crate::BrowserTestRunner`], the step's duration is also added to the test's
/// [`crate::BrowserTestRecord::steps`] under `kind`, and the run summary lists the step kinds that
/// took the most time.
///
/// Use a small, fixed set of `kind`s (e.g. `"goto"`, `"wait_for_text"`), so that they aggregate
/// across tests, and put the specifics (URL, selector, expected text) into `detail`, which is only
/// logged. Steps may nest; a nested step's time counts towards every enclosing kind as well.
///
/// Instrument your page-object helpers with this function to see where your tests spend time.
///
/// # Examples
///
/// ```rust,no_run
/// # use browser_test::thirtyfour::{WebDriver, error::WebDriverResult};
/// async fn goto(driver: &WebDriver, url: &str) -> WebDriverResult<()> {
///     browser_test::step("goto", url, driver.goto(url)).await
/// }
/// ```
pub async fn step<F: Future>(
    kind: impl Into<Cow<'static, str>>,
    detail: impl Display,
    future: F,
) -> F::Output {
    let kind = kind.into();
    let start = Instant::now();
    let output = future.await;
    let duration = start.elapsed();

    let recorder = CURRENT_TEST.try_with(Arc::clone).ok();
    let slow_step = match &recorder {
        Some(recorder) => recorder.slow_step,
        None => Some(DEFAULT_SLOW_STEP),
    };
    if let Some(recorder) = &recorder {
        recorder.record(&kind, duration);
    }
    if slow_step.is_some_and(|slow_step| duration > slow_step) {
        tracing::warn!(
            step = %kind,
            duration_ms = duration.as_millis(),
            "Slow step: {kind} {detail} took {}",
            FormatDuration(duration),
        );
    } else {
        tracing::debug!(
            step = %kind,
            duration_ms = duration.as_millis(),
            "Step {kind} {detail} took {}",
            FormatDuration(duration),
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use assertr::prelude::*;

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
                    step("goto", "/a", async {}).await;
                    step("goto", "/b", async {}).await;
                }),
                second.scope(async {
                    step("wait", "#id", async {}).await;
                }),
            )
            .await;
            // Outside of a test, steps are only logged.
            step("goto", "/c", async {}).await;
        });

        let first = first.take();
        let second = second.take();
        assert_that!(first.keys().collect::<Vec<_>>()).is_equal_to(vec!["goto"]);
        assert_that!(first["goto"].count).is_equal_to(2);
        assert_that!(second.keys().collect::<Vec<_>>()).is_equal_to(vec!["wait"]);
        assert_that!(second["wait"].count).is_equal_to(1);
    }

    #[test]
    fn step_returns_the_future_output() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime should build");

        let output = runtime.block_on(step("compute", "answer", async { 42 }));

        assert_that!(output).is_equal_to(42);
    }
}

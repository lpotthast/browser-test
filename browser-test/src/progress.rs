//! Warnings about browser tests that make no progress.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use crate::{
    report::FormatDuration,
    step::{DEFAULT_SLOW_STEP, StepRecorder},
};

/// When [`crate::BrowserTestRunner`] warns that a test run seems not to progress fast enough.
///
/// Slow progress often hints at a slowed-down system (e.g. a busy CI machine) or at a test waiting
/// for something that never happens. Warnings are logged through `tracing` at `warn` level.
///
/// Every warning can be disabled on its own, with the `without_*` methods.
///
/// # Examples
///
/// ```rust
/// use std::time::Duration;
///
/// use browser_test::{BrowserTestRunner, Cancellation, ProgressWarnings};
///
/// let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals()).with_progress_warnings(
///     ProgressWarnings::default()
///         .with_test_running(Duration::from_secs(60))
///         .without_slow_step(),
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgressWarnings {
    test_running: Option<Duration>,
    session: Option<Duration>,
    slow_step: Option<Duration>,
}

impl Default for ProgressWarnings {
    /// Warn about tests running for 30 seconds, sessions taking 5 seconds to create, reset or
    /// quit, and steps taking 2 seconds.
    fn default() -> Self {
        Self {
            test_running: Some(Duration::from_secs(30)),
            session: Some(Duration::from_secs(5)),
            slow_step: Some(DEFAULT_SLOW_STEP),
        }
    }
}

impl ProgressWarnings {
    /// Never warn.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            test_running: None,
            session: None,
            slow_step: None,
        }
    }

    /// Warn when a test body is still running after `threshold`, and again every time
    /// `threshold` passes. A warning names the innermost [`crate::Step`] that is running, if any,
    /// and for how long: a step that never ends is otherwise only logged once the test's time limit
    /// drops it. Defaults to 30 seconds.
    #[must_use]
    pub const fn with_test_running(mut self, threshold: Duration) -> Self {
        self.test_running = Some(threshold);
        self
    }

    /// Never warn about test bodies that are still running.
    #[must_use]
    pub const fn without_test_running(mut self) -> Self {
        self.test_running = None;
        self
    }

    /// Warn when creating, resetting or quitting a session takes longer than `threshold`.
    /// Defaults to 5 seconds.
    #[must_use]
    pub const fn with_session(mut self, threshold: Duration) -> Self {
        self.session = Some(threshold);
        self
    }

    /// Never warn about slow sessions.
    #[must_use]
    pub const fn without_session(mut self) -> Self {
        self.session = None;
        self
    }

    /// Warn when a [`crate::Step`] takes longer than `threshold`. Defaults to 2 seconds.
    #[must_use]
    pub const fn with_slow_step(mut self, threshold: Duration) -> Self {
        self.slow_step = Some(threshold);
        self
    }

    /// Never warn about slow [`crate::Step`]s.
    #[must_use]
    pub const fn without_slow_step(mut self) -> Self {
        self.slow_step = None;
        self
    }

    /// Threshold for test bodies that are still running, `None` if these warnings are disabled.
    #[must_use]
    pub const fn test_running(self) -> Option<Duration> {
        self.test_running
    }

    /// Threshold for creating, resetting and quitting sessions, `None` if these warnings are
    /// disabled.
    #[must_use]
    pub const fn session(self) -> Option<Duration> {
        self.session
    }

    /// Threshold for [`crate::Step`]s, `None` if these warnings are disabled.
    #[must_use]
    pub const fn slow_step(self) -> Option<Duration> {
        self.slow_step
    }

    const fn threshold(self, phase: Phase) -> Option<Duration> {
        match phase {
            Phase::RunningTest => self.test_running,
            Phase::CreatingSession | Phase::ResettingSession | Phase::QuittingSession => {
                self.session
            }
        }
    }

    /// How often the watchdog checks for overdue phases.
    fn tick(self) -> Option<Duration> {
        [self.test_running, self.session]
            .into_iter()
            .flatten()
            .min()
            .map(|min| (min / 4).clamp(Duration::from_millis(50), Duration::from_secs(1)))
    }
}

/// What the runner is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    CreatingSession,
    RunningTest,
    /// Resetting a test's session for the next test (see [`crate::SessionReuse`]).
    ResettingSession,
    QuittingSession,
}

/// Something the runner does that the watchdog follows: a test, or a session of the session pool
/// that is not yet assigned to a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Activity {
    /// The test with the given index.
    Test(usize),
    /// The pool session with the given number.
    Session(usize),
}

#[derive(Debug)]
struct ActivityStatus {
    /// What the warning is about, e.g. `browser test 'login'`.
    subject: String,
    phase: Phase,
    since: Instant,
    warnings: u32,
    /// The steps of a running test, whose running step warnings name.
    steps: Option<Arc<StepRecorder>>,
}

impl ActivityStatus {
    /// The warning that the activity is still in its phase after `elapsed`.
    fn still_in_phase(&self, elapsed: Duration) -> String {
        let doing = match self.phase {
            Phase::CreatingSession => "creating",
            Phase::RunningTest => "running",
            Phase::ResettingSession => "resetting the session of",
            Phase::QuittingSession => "quitting the session of",
        };
        let step = self
            .steps
            .as_ref()
            .and_then(|steps| steps.running_step())
            .map(|step| format!(", in step {step}"))
            .unwrap_or_default();
        format!(
            "Still {doing} {} after {}{step}.",
            self.subject,
            FormatDuration(elapsed)
        )
    }
}

/// What the runner is currently doing, for the watchdog.
#[derive(Debug, Default)]
pub(crate) struct ProgressBoard {
    activities: Mutex<BTreeMap<Activity, ActivityStatus>>,
}

impl ProgressBoard {
    /// Record that `activity` entered `phase`. `subject` names it in warnings.
    pub(crate) fn enter(&self, activity: Activity, subject: impl Into<String>, phase: Phase) {
        self.insert(activity, subject.into(), phase, None);
    }

    /// Record that `activity` started running its test, whose steps `steps` records. Warnings
    /// name the step that is running.
    pub(crate) fn enter_running_test(
        &self,
        activity: Activity,
        subject: impl Into<String>,
        steps: &Arc<StepRecorder>,
    ) {
        self.insert(
            activity,
            subject.into(),
            Phase::RunningTest,
            Some(Arc::clone(steps)),
        );
    }

    fn insert(
        &self,
        activity: Activity,
        subject: String,
        phase: Phase,
        steps: Option<Arc<StepRecorder>>,
    ) {
        self.activities
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                activity,
                ActivityStatus {
                    subject,
                    phase,
                    since: Instant::now(),
                    warnings: 0,
                    steps,
                },
            );
    }

    pub(crate) fn clear(&self, activity: Activity) {
        self.activities
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&activity);
    }

    /// Log a warning for every activity whose current phase exceeded its threshold once more.
    fn warn_overdue(&self, warnings: ProgressWarnings, now: Instant) {
        let mut activities = self
            .activities
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for status in activities.values_mut() {
            let Some(threshold) = warnings.threshold(status.phase) else {
                continue;
            };
            let elapsed = now.saturating_duration_since(status.since);
            if is_overdue(elapsed, threshold, status.warnings) {
                status.warnings += 1;
                tracing::warn!(
                    subject = %status.subject,
                    elapsed_ms = elapsed.as_millis(),
                    "{}",
                    status.still_in_phase(elapsed),
                );
            }
        }
    }

    /// Warn about overdue phases until the returned future is dropped.
    pub(crate) async fn watch(&self, warnings: ProgressWarnings) {
        let Some(tick) = warnings.tick() else {
            return std::future::pending().await;
        };
        loop {
            tokio::time::sleep(tick).await;
            self.warn_overdue(warnings, Instant::now());
        }
    }
}

/// Whether a phase running for `elapsed` deserves another warning, after `warnings` were logged.
/// Warnings repeat every `threshold`.
fn is_overdue(elapsed: Duration, threshold: Duration, warnings: u32) -> bool {
    !threshold.is_zero() && elapsed >= threshold.saturating_mul(warnings + 1)
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    #[test]
    fn defaults_warn_about_long_tests_and_slow_sessions() {
        let warnings = ProgressWarnings::default();

        assert_that!(warnings.test_running()).is_equal_to(Some(Duration::from_secs(30)));
        assert_that!(warnings.session()).is_equal_to(Some(Duration::from_secs(5)));
        assert_that!(warnings.slow_step()).is_equal_to(Some(Duration::from_secs(2)));
        assert_that!(warnings.tick()).is_equal_to(Some(Duration::from_secs(1)));
    }

    #[test]
    fn disabled_never_ticks() {
        assert_that!(ProgressWarnings::disabled().tick()).is_none();
        assert_that!(
            ProgressWarnings::default()
                .without_test_running()
                .without_session()
                .without_slow_step()
        )
        .is_equal_to(ProgressWarnings::disabled());
    }

    #[test]
    fn tick_follows_the_smallest_threshold() {
        let warnings = ProgressWarnings::default()
            .with_test_running(Duration::from_millis(400))
            .without_session();

        assert_that!(warnings.tick()).is_equal_to(Some(Duration::from_millis(100)));
    }

    #[test]
    fn overdue_warnings_repeat_every_threshold() {
        let threshold = Duration::from_secs(30);

        assert_that!(is_overdue(Duration::from_secs(29), threshold, 0)).is_false();
        assert_that!(is_overdue(Duration::from_secs(30), threshold, 0)).is_true();
        assert_that!(is_overdue(Duration::from_secs(45), threshold, 1)).is_false();
        assert_that!(is_overdue(Duration::from_secs(60), threshold, 1)).is_true();
        assert_that!(is_overdue(Duration::from_secs(60), Duration::ZERO, 0)).is_false();
    }

    #[test]
    fn board_counts_warnings_per_phase() {
        let board = ProgressBoard::default();
        let warnings = ProgressWarnings::default();
        let test = Activity::Test(1);
        board.enter(test, "browser test 'slow test'", Phase::RunningTest);
        let since = board.activities.lock().unwrap()[&test].since;

        board.warn_overdue(warnings, since + Duration::from_secs(31));
        board.warn_overdue(warnings, since + Duration::from_secs(32));

        assert_that!(board.activities.lock().unwrap()[&test].warnings).is_equal_to(1);

        board.enter(test, "browser test 'slow test'", Phase::QuittingSession);
        assert_that!(board.activities.lock().unwrap()[&test].warnings).is_equal_to(0);
        board.clear(test);
        assert_that!(*board.activities.lock().unwrap()).does_not_contain_key(&test);
    }

    #[test]
    fn warnings_about_running_tests_name_the_running_step() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime should build");
        let board = ProgressBoard::default();
        let test = Activity::Test(1);
        let steps = StepRecorder::new(None);
        board.enter_running_test(test, "browser test 'login'", &steps);
        let warning =
            || board.activities.lock().unwrap()[&test].still_in_phase(Duration::from_secs(30));
        assert_that!(warning())
            .is_equal_to("Still running browser test 'login' after 30.00s.".to_owned());

        runtime.block_on(steps.scope(async {
            use crate::StepExt as _;
            let mut wait = Box::pin(std::future::pending::<()>().step("wait").detail("#status"));
            assert_that!(futures_util::poll!(wait.as_mut()).is_pending()).is_true();
            assert_that!(warning()).starts_with(
                "Still running browser test 'login' after 30.00s, in step wait #status for ",
            );
        }));
    }
}

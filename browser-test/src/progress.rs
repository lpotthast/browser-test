//! Warnings about browser tests that make no progress.

use std::{
    collections::BTreeMap,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use crate::{report::FormatDuration, step::DEFAULT_SLOW_STEP};

/// When [`crate::BrowserTestRunner`] warns that a test run seems not to progress fast enough.
///
/// Slow progress often hints at a slowed-down system (e.g. a busy CI machine) or at a test waiting
/// for something that never happens. Warnings are logged through `tracing` at `warn` level.
///
/// Every threshold is optional: `None` disables its warnings.
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
///         .with_test_running(Some(Duration::from_secs(60)))
///         .with_slow_step(None),
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
    /// `threshold` passes. `None` disables these warnings. Defaults to 30 seconds.
    #[must_use]
    pub const fn with_test_running(mut self, threshold: Option<Duration>) -> Self {
        self.test_running = threshold;
        self
    }

    /// Warn when creating, resetting or quitting a session takes longer than `threshold`. `None`
    /// disables these warnings. Defaults to 5 seconds.
    #[must_use]
    pub const fn with_session(mut self, threshold: Option<Duration>) -> Self {
        self.session = threshold;
        self
    }

    /// Warn when a [`crate::Step`] takes longer than `threshold`. `None` disables these warnings.
    /// Defaults to 2 seconds.
    #[must_use]
    pub const fn with_slow_step(mut self, threshold: Option<Duration>) -> Self {
        self.slow_step = threshold;
        self
    }

    /// Threshold for test bodies that are still running.
    #[must_use]
    pub const fn test_running(self) -> Option<Duration> {
        self.test_running
    }

    /// Threshold for creating, resetting and quitting sessions.
    #[must_use]
    pub const fn session(self) -> Option<Duration> {
        self.session
    }

    /// Threshold for [`crate::Step`]s.
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
}

/// What the runner is currently doing, for the watchdog.
#[derive(Debug, Default)]
pub(crate) struct ProgressBoard {
    activities: Mutex<BTreeMap<Activity, ActivityStatus>>,
}

impl ProgressBoard {
    /// Record that `activity` entered `phase`. `subject` names it in warnings.
    pub(crate) fn enter(&self, activity: Activity, subject: impl Into<String>, phase: Phase) {
        self.activities
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                activity,
                ActivityStatus {
                    subject: subject.into(),
                    phase,
                    since: Instant::now(),
                    warnings: 0,
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
                let doing = match status.phase {
                    Phase::CreatingSession => "creating",
                    Phase::RunningTest => "running",
                    Phase::ResettingSession => "resetting the session of",
                    Phase::QuittingSession => "quitting the session of",
                };
                tracing::warn!(
                    subject = %status.subject,
                    elapsed_ms = elapsed.as_millis(),
                    "Still {doing} {} after {}.",
                    status.subject,
                    FormatDuration(elapsed),
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
    }

    #[test]
    fn tick_follows_the_smallest_threshold() {
        let warnings = ProgressWarnings::default()
            .with_test_running(Some(Duration::from_millis(400)))
            .with_session(None);

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
        assert_that!(board.activities.lock().unwrap().contains_key(&test)).is_false();
    }
}

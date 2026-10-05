//! Timing report of a browser test run.

use std::collections::BTreeMap;
use std::fmt::{self, Display, Write as _};
use std::time::Duration;

use rootcause::Report;

use crate::BrowserTestError;

/// What [`crate::BrowserTestRunner::run_with_report`] returns: the run's result and its timing
/// report.
#[derive(Debug)]
pub struct BrowserTestRunOutcome {
    /// The run's result, as [`crate::BrowserTestRunner::run`] returns it.
    pub result: Result<(), Report<BrowserTestError>>,

    /// Where the run spent its time.
    pub report: BrowserTestRunReport,
}

impl BrowserTestRunOutcome {
    /// The run's result, dropping the report.
    ///
    /// # Errors
    ///
    /// Returns the run's error, see [`crate::BrowserTestRunner::run`].
    pub fn into_result(self) -> Result<(), Report<BrowserTestError>> {
        self.result
    }
}

/// Where a browser test run spent its time.
///
/// Printed as a summary at the end of every run (see
/// [`crate::BrowserTestRunner::with_run_summary`]); its `Display` implementation renders that
/// summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BrowserTestRunReport {
    /// Wall time of the whole run, from the start of [`crate::BrowserTestRunner::run`] until it
    /// returned. Includes the manual pause, if one was enabled.
    pub total: Duration,

    /// Time spent resolving Chrome for Testing and starting chromedriver.
    pub webdriver_startup: Duration,

    /// Time spent terminating chromedriver.
    pub webdriver_shutdown: Duration,

    /// Every test that was started (or failed before it could start), in the order the tests were
    /// given to the runner. Tests not started because of [`crate::BrowserTestFailurePolicy::FailFast`]
    /// are missing.
    pub tests: Vec<BrowserTestRecord>,
}

/// Timing and outcome of one browser test.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BrowserTestRecord {
    /// Position of the test in the collection given to the runner.
    pub index: usize,

    /// The test's name.
    pub name: String,

    /// Whether the test passed.
    pub outcome: TestOutcome,

    /// The parallel slot that ran the test (always `0` for sequential runs).
    pub slot: usize,

    /// How the test got its session, and how long that took.
    pub session: SessionAcquisition,

    /// Time spent in [`crate::BrowserTest::run`]. `None` if the body never ran (e.g. the session
    /// could not be created).
    pub body: Option<Duration>,

    /// Time spent quitting the session after this test, if this test was the last one to use it.
    pub teardown: Option<Duration>,

    /// Time spent in [`crate::step`]s of this test, per step kind.
    pub steps: BTreeMap<String, StepStats>,
}

impl BrowserTestRecord {
    /// Total time attributed to this test: session acquisition, body, and teardown.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.session.duration() + self.body.unwrap_or_default() + self.teardown.unwrap_or_default()
    }
}

/// Outcome of one browser test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestOutcome {
    /// The test passed.
    Passed,

    /// The test (or its session) failed.
    Failed,

    /// The test panicked.
    Panicked,
}

/// How a test got its `WebDriver` session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionAcquisition {
    /// A new session was created for this test.
    Created {
        /// Time to create the session and apply its settings (successful or not).
        duration: Duration,
    },

    /// The test reused the session of an earlier test.
    Reused {
        /// Time spent resetting the session before this test.
        reset: Duration,
    },

    /// The test failed before a session was requested (e.g. one of its metadata methods panicked).
    None,
}

impl SessionAcquisition {
    /// Time spent acquiring the session.
    #[must_use]
    pub const fn duration(self) -> Duration {
        match self {
            Self::Created { duration } => duration,
            Self::Reused { reset } => reset,
            Self::None => Duration::ZERO,
        }
    }
}

/// Aggregated timing of one kind of [`crate::step`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StepStats {
    /// How many steps of this kind ran.
    pub count: u32,

    /// Their summed duration. Nested steps count towards every enclosing kind as well.
    pub total: Duration,

    /// The longest step of this kind.
    pub max: Duration,
}

impl StepStats {
    pub(crate) fn record(&mut self, duration: Duration) {
        self.count += 1;
        self.total += duration;
        self.max = self.max.max(duration);
    }

    fn merge(&mut self, other: Self) {
        self.count += other.count;
        self.total += other.total;
        self.max = self.max.max(other.max);
    }
}

/// How many entries the summary's lists show.
const SUMMARY_LIST_LEN: usize = 10;

impl BrowserTestRunReport {
    /// Number of sessions created.
    #[must_use]
    pub fn sessions_created(&self) -> usize {
        self.tests
            .iter()
            .filter(|test| matches!(test.session, SessionAcquisition::Created { .. }))
            .count()
    }

    /// Number of tests that reused a session.
    #[must_use]
    pub fn sessions_reused(&self) -> usize {
        self.tests
            .iter()
            .filter(|test| matches!(test.session, SessionAcquisition::Reused { .. }))
            .count()
    }

    /// Total time spent creating sessions.
    #[must_use]
    pub fn session_creation_time(&self) -> Duration {
        self.tests
            .iter()
            .filter_map(|test| match test.session {
                SessionAcquisition::Created { duration } => Some(duration),
                _ => None,
            })
            .sum()
    }

    /// Total time spent resetting sessions for reuse.
    #[must_use]
    pub fn session_reset_time(&self) -> Duration {
        self.tests
            .iter()
            .filter_map(|test| match test.session {
                SessionAcquisition::Reused { reset } => Some(reset),
                _ => None,
            })
            .sum()
    }

    /// Total time spent quitting sessions.
    #[must_use]
    pub fn session_teardown_time(&self) -> Duration {
        self.tests.iter().filter_map(|test| test.teardown).sum()
    }

    /// Total time spent in test bodies.
    #[must_use]
    pub fn body_time(&self) -> Duration {
        self.tests.iter().filter_map(|test| test.body).sum()
    }

    /// The tests sorted by [`BrowserTestRecord::total`], slowest first.
    #[must_use]
    pub fn slowest_tests(&self) -> Vec<&BrowserTestRecord> {
        let mut tests: Vec<_> = self.tests.iter().collect();
        tests.sort_by_key(|test| std::cmp::Reverse(test.total()));
        tests
    }

    /// The [`crate::step`] kinds of all tests, aggregated and sorted by total time, slowest first.
    #[must_use]
    pub fn slowest_steps(&self) -> Vec<(String, StepStats)> {
        let mut steps = BTreeMap::<String, StepStats>::new();
        for test in &self.tests {
            for (kind, stats) in &test.steps {
                steps.entry(kind.clone()).or_default().merge(*stats);
            }
        }
        let mut steps: Vec<_> = steps.into_iter().collect();
        steps.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.total));
        steps
    }

    fn count(&self, outcome: TestOutcome) -> usize {
        self.tests
            .iter()
            .filter(|test| test.outcome == outcome)
            .count()
    }
}

impl Display for BrowserTestRunReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let passed = self.count(TestOutcome::Passed);
        let failed = self.count(TestOutcome::Failed) + self.count(TestOutcome::Panicked);
        writeln!(
            f,
            "Browser test run: {} test(s), {passed} passed, {failed} failed, in {}",
            self.tests.len(),
            FormatDuration(self.total),
        )?;
        writeln!(
            f,
            "  chromedriver:   started in {}, stopped in {}",
            FormatDuration(self.webdriver_startup),
            FormatDuration(self.webdriver_shutdown),
        )?;
        let created = self.sessions_created();
        let reused = self.sessions_reused();
        writeln!(
            f,
            "  sessions:       {created} created in {}{}, {reused} reused after resets taking {}{}, quit in {}",
            FormatDuration(self.session_creation_time()),
            Average(self.session_creation_time(), created),
            FormatDuration(self.session_reset_time()),
            Average(self.session_reset_time(), reused),
            FormatDuration(self.session_teardown_time()),
        )?;
        writeln!(f, "  test bodies:    {}", FormatDuration(self.body_time()))?;

        let slowest = self.slowest_tests();
        if !slowest.is_empty() {
            writeln!(f, "  slowest tests:")?;
            for test in slowest.into_iter().take(SUMMARY_LIST_LEN) {
                writeln!(
                    f,
                    "    {:>9}  {}",
                    FormatDuration(test.total()),
                    Describe(test)
                )?;
            }
        }

        let steps = self.slowest_steps();
        if !steps.is_empty() {
            writeln!(f, "  slowest steps (by total time):")?;
            for (kind, stats) in steps.into_iter().take(SUMMARY_LIST_LEN) {
                writeln!(
                    f,
                    "    {:>9}  {kind}: {}x, max {}",
                    FormatDuration(stats.total),
                    stats.count,
                    FormatDuration(stats.max),
                )?;
            }
        }
        Ok(())
    }
}

/// One test of the summary: name, outcome, and how its time splits up.
struct Describe<'a>(&'a BrowserTestRecord);

impl Display for Describe<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let test = self.0;
        f.write_str(&test.name)?;
        match test.outcome {
            TestOutcome::Passed => {}
            TestOutcome::Failed => f.write_str(" [failed]")?,
            TestOutcome::Panicked => f.write_str(" [panicked]")?,
        }
        f.write_str(" (")?;
        f.write_str(&timing_breakdown(test))?;
        f.write_str(")")
    }
}

/// How a test's time splits up, e.g. `new session 812ms, body 3.20s, quit 40ms`.
pub(crate) fn timing_breakdown(test: &BrowserTestRecord) -> String {
    let mut out = String::new();
    match test.session {
        SessionAcquisition::Created { duration } => {
            let _ = write!(out, "new session {}", FormatDuration(duration));
        }
        SessionAcquisition::Reused { reset } => {
            let _ = write!(out, "reused session, reset {}", FormatDuration(reset));
        }
        SessionAcquisition::None => out.push_str("no session"),
    }
    if let Some(body) = test.body {
        let _ = write!(out, ", body {}", FormatDuration(body));
    }
    if let Some(teardown) = test.teardown {
        let _ = write!(out, ", quit {}", FormatDuration(teardown));
    }
    out
}

struct Average(Duration, usize);

impl Display for Average {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(total, count) = *self;
        match u32::try_from(count) {
            Ok(count) if count > 0 => write!(f, " (avg {})", FormatDuration(total / count)),
            _ => Ok(()),
        }
    }
}

/// Human-readable duration: `850ms`, `3.25s`, `2m 05.3s`.
pub(crate) struct FormatDuration(pub(crate) Duration);

impl Display for FormatDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let duration = self.0;
        let text = if duration < Duration::from_secs(1) {
            format!("{}ms", duration.as_millis())
        } else if duration < Duration::from_secs(60) {
            format!("{:.2}s", duration.as_secs_f64())
        } else {
            let minutes = duration.as_secs() / 60;
            let seconds = duration.as_secs_f64() % 60.0;
            format!("{minutes}m {seconds:04.1}s")
        };
        f.pad(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assertr::prelude::*;

    fn record(
        index: usize,
        name: &str,
        session: SessionAcquisition,
        body_ms: u64,
    ) -> BrowserTestRecord {
        BrowserTestRecord {
            index,
            name: name.to_owned(),
            outcome: TestOutcome::Passed,
            slot: 0,
            session,
            body: Some(Duration::from_millis(body_ms)),
            teardown: None,
            steps: BTreeMap::new(),
        }
    }

    fn created(ms: u64) -> SessionAcquisition {
        SessionAcquisition::Created {
            duration: Duration::from_millis(ms),
        }
    }

    fn reused(ms: u64) -> SessionAcquisition {
        SessionAcquisition::Reused {
            reset: Duration::from_millis(ms),
        }
    }

    #[test]
    fn format_duration_picks_unit_by_magnitude() {
        assert_that!(FormatDuration(Duration::from_millis(850)).to_string()).is_equal_to("850ms");
        assert_that!(FormatDuration(Duration::from_millis(3250)).to_string()).is_equal_to("3.25s");
        assert_that!(FormatDuration(Duration::from_millis(125_300)).to_string())
            .is_equal_to("2m 05.3s");
    }

    #[test]
    fn report_aggregates_sessions_and_bodies() {
        let mut slow = record(1, "slow", reused(20), 5000);
        slow.teardown = Some(Duration::from_millis(30));
        let report = BrowserTestRunReport {
            tests: vec![
                record(0, "fast", created(800), 100),
                slow,
                record(2, "isolated", created(700), 300),
            ],
            ..BrowserTestRunReport::default()
        };

        assert_that!(report.sessions_created()).is_equal_to(2);
        assert_that!(report.sessions_reused()).is_equal_to(1);
        assert_that!(report.session_creation_time()).is_equal_to(Duration::from_millis(1500));
        assert_that!(report.session_reset_time()).is_equal_to(Duration::from_millis(20));
        assert_that!(report.session_teardown_time()).is_equal_to(Duration::from_millis(30));
        assert_that!(report.body_time()).is_equal_to(Duration::from_millis(5400));
        let slowest: Vec<_> = report
            .slowest_tests()
            .into_iter()
            .map(|test| test.name.as_str())
            .collect();
        assert_that!(slowest).is_equal_to(vec!["slow", "isolated", "fast"]);
    }

    #[test]
    fn report_aggregates_steps_across_tests() {
        let mut first = record(0, "first", created(1), 1);
        first.steps.insert(
            "goto".to_owned(),
            StepStats {
                count: 2,
                total: Duration::from_millis(300),
                max: Duration::from_millis(200),
            },
        );
        let mut second = record(1, "second", reused(1), 1);
        second.steps.insert(
            "goto".to_owned(),
            StepStats {
                count: 1,
                total: Duration::from_millis(500),
                max: Duration::from_millis(500),
            },
        );
        second.steps.insert(
            "wait".to_owned(),
            StepStats {
                count: 1,
                total: Duration::from_millis(100),
                max: Duration::from_millis(100),
            },
        );
        let report = BrowserTestRunReport {
            tests: vec![first, second],
            ..BrowserTestRunReport::default()
        };

        let steps = report.slowest_steps();

        assert_that!(steps.len()).is_equal_to(2);
        assert_that!(steps[0].0.as_str()).is_equal_to("goto");
        assert_that!(steps[0].1).is_equal_to(StepStats {
            count: 3,
            total: Duration::from_millis(800),
            max: Duration::from_millis(500),
        });
        assert_that!(steps[1].0.as_str()).is_equal_to("wait");
    }

    #[test]
    fn summary_lists_sessions_and_slowest_tests() {
        let mut failed = record(1, "broken", reused(15), 2500);
        failed.outcome = TestOutcome::Failed;
        failed.teardown = Some(Duration::from_millis(40));
        let report = BrowserTestRunReport {
            total: Duration::from_secs(5),
            webdriver_startup: Duration::from_millis(300),
            webdriver_shutdown: Duration::from_millis(50),
            tests: vec![record(0, "works", created(800), 1000), failed],
        };

        let summary = report.to_string();

        assert_that!(summary.as_str())
            .contains("Browser test run: 2 test(s), 1 passed, 1 failed, in 5.00s");
        assert_that!(summary.as_str()).contains(
            "1 created in 800ms (avg 800ms), 1 reused after resets taking 15ms (avg 15ms), quit in 40ms",
        );
        assert_that!(summary.as_str())
            .contains("broken [failed] (reused session, reset 15ms, body 2.50s, quit 40ms)");
        let broken = summary.find("broken").expect("summary lists the slow test");
        let works = summary.find("works").expect("summary lists the fast test");
        assert_that!(broken < works).is_true();
    }
}

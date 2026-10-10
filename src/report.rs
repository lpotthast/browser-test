//! Timing report of a browser test run.

use std::collections::BTreeMap;
use std::fmt::{self, Display, Write as _};
use std::time::Duration;

/// Where a browser test run spent its time.
///
/// Handed to the [`crate::RunReportConsumer`]s of the runner at the end of every run. Its `Display`
/// implementation renders a human-readable summary, as printed by [`crate::StderrSummary`].
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
    /// given to the runner. Tests not started because of [`crate::FailurePolicy::FailFast`]
    /// are missing.
    pub tests: Vec<BrowserTestRecord>,

    /// Every named group (see [`crate::BrowserTests::named`]) that ran, in the order the groups
    /// were defined.
    pub groups: Vec<GroupRecord>,
}

/// Timing of one named group of tests.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroupRecord {
    /// The group's name, prefixed with the names of enclosing named groups (`outer / inner`).
    pub name: String,

    /// Wall time from the group's start until its last test finished.
    pub duration: Duration,
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

    /// The name of the innermost named group containing the test (see [`GroupRecord::name`]).
    pub group: Option<String>,

    /// How the test's session was prepared (created, or reset after an earlier test), and how long
    /// the test waited for it. `None` if no session was requested (e.g. one of the test's metadata
    /// methods panicked).
    pub session: Option<SessionTiming>,

    /// Time spent in [`crate::BrowserTest::run`]. `None` if the body never ran (e.g. the session
    /// could not be created).
    pub body: Option<Duration>,

    /// Time spent quitting the session after the test. Quitting runs in the background, while the
    /// next test already runs. `None` if the session was kept to run another test (see
    /// [`crate::SessionReuse`]; resetting it counts towards that test's
    /// [`SessionPreparation::Reset`]), or if it could not be created or set up.
    pub teardown: Option<Duration>,

    /// Time spent in [`crate::Step`]s of this test, per step kind.
    pub steps: BTreeMap<String, StepStats>,
}

impl BrowserTestRecord {
    /// Time this test ran for: waiting for its session, then running its body.
    ///
    /// Session creation (ahead of the test) and teardown (after it) run in the background and
    /// are not included.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.session.map(|session| session.wait).unwrap_or_default() + self.body.unwrap_or_default()
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

/// Timing of the `WebDriver` session of one test.
///
/// The runner prepares sessions ahead of the tests that use them (see
/// [`crate::BrowserTestRunner::with_spare_sessions`]), so preparing them usually overlaps earlier
/// tests and only [`Self::wait`] delays the test itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SessionTiming {
    /// How the session was prepared for the test, and how long that took.
    pub preparation: SessionPreparation,

    /// Time the test waited for its session once it was its turn to run. Zero if the session was
    /// ready in time.
    pub wait: Duration,
}

/// How the session of a test was prepared, and how long that took (successful or not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionPreparation {
    /// The session was created: a new browser started, and its page brought to the front.
    Created(Duration),

    /// The session ran earlier tests and was reset after the last of them (see
    /// [`crate::SessionReuse`]).
    Reset(Duration),
}

impl SessionPreparation {
    /// How long preparing the session took.
    #[must_use]
    pub const fn duration(self) -> Duration {
        match self {
            Self::Created(duration) | Self::Reset(duration) => duration,
        }
    }
}

/// Aggregated timing of one kind of [`crate::Step`].
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
    /// Number of sessions created for the tests of this report.
    #[must_use]
    pub fn sessions_created(&self) -> usize {
        self.created().count()
    }

    /// Total time spent creating sessions. Mostly overlaps earlier tests.
    #[must_use]
    pub fn session_creation_time(&self) -> Duration {
        self.created().sum()
    }

    /// Number of tests that ran in the reset session of an earlier test (see
    /// [`crate::SessionReuse`]).
    #[must_use]
    pub fn session_resets(&self) -> usize {
        self.resets().count()
    }

    /// Total time spent resetting sessions for further tests. Mostly overlaps earlier tests.
    #[must_use]
    pub fn session_reset_time(&self) -> Duration {
        self.resets().sum()
    }

    fn created(&self) -> impl Iterator<Item = Duration> {
        self.tests
            .iter()
            .filter_map(|test| match test.session?.preparation {
                SessionPreparation::Created(duration) => Some(duration),
                SessionPreparation::Reset(_) => None,
            })
    }

    fn resets(&self) -> impl Iterator<Item = Duration> {
        self.tests
            .iter()
            .filter_map(|test| match test.session?.preparation {
                SessionPreparation::Reset(duration) => Some(duration),
                SessionPreparation::Created(_) => None,
            })
    }

    /// Total time tests waited for their sessions, i.e. the part of session creation that did not
    /// overlap earlier tests.
    #[must_use]
    pub fn session_wait_time(&self) -> Duration {
        self.tests
            .iter()
            .filter_map(|test| test.session)
            .map(|session| session.wait)
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

    /// The [`crate::Step`] kinds of all tests, aggregated and sorted by total time, slowest first.
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
        write!(
            f,
            "  sessions:       {created} created in {}{}",
            FormatDuration(self.session_creation_time()),
            Average(self.session_creation_time(), created),
        )?;
        let resets = self.session_resets();
        if resets > 0 {
            write!(
                f,
                ", reset {resets}x in {}{}",
                FormatDuration(self.session_reset_time()),
                Average(self.session_reset_time(), resets),
            )?;
        }
        writeln!(
            f,
            ", tests waited {} for them, quit in {}",
            FormatDuration(self.session_wait_time()),
            FormatDuration(self.session_teardown_time()),
        )?;
        writeln!(f, "  test bodies:    {}", FormatDuration(self.body_time()))?;
        if !self.groups.is_empty() {
            writeln!(f, "  groups:")?;
            for group in &self.groups {
                writeln!(
                    f,
                    "    {:>9}  {}",
                    FormatDuration(group.duration),
                    group.name
                )?;
            }
        }

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
                    "    {:>9}  {kind}: {}x{}, max {}",
                    FormatDuration(stats.total),
                    stats.count,
                    Average(stats.total, stats.count as usize),
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

/// How a test's time splits up, e.g. `session 812ms, waited 120ms, body 3.20s, quit 40ms`, or
/// `reset 60ms, body 1.10s` in a reused session.
pub(crate) fn timing_breakdown(test: &BrowserTestRecord) -> String {
    let mut out = String::new();
    match test.session {
        Some(session) => {
            let _ = match session.preparation {
                SessionPreparation::Created(duration) => {
                    write!(out, "session {}", FormatDuration(duration))
                }
                SessionPreparation::Reset(duration) => {
                    write!(out, "reset {}", FormatDuration(duration))
                }
            };
            if !session.wait.is_zero() {
                let _ = write!(out, ", waited {}", FormatDuration(session.wait));
            }
        }
        None => out.push_str("no session"),
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

    fn record(index: usize, name: &str, session: SessionTiming, body_ms: u64) -> BrowserTestRecord {
        BrowserTestRecord {
            index,
            name: name.to_owned(),
            outcome: TestOutcome::Passed,
            group: None,
            session: Some(session),
            body: Some(Duration::from_millis(body_ms)),
            teardown: None,
            steps: BTreeMap::new(),
        }
    }

    fn session(creation_ms: u64, wait_ms: u64) -> SessionTiming {
        SessionTiming {
            preparation: SessionPreparation::Created(Duration::from_millis(creation_ms)),
            wait: Duration::from_millis(wait_ms),
        }
    }

    fn reset(reset_ms: u64, wait_ms: u64) -> SessionTiming {
        SessionTiming {
            preparation: SessionPreparation::Reset(Duration::from_millis(reset_ms)),
            wait: Duration::from_millis(wait_ms),
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
        let mut slow = record(1, "slow", session(300, 0), 5000);
        slow.teardown = Some(Duration::from_millis(30));
        let mut broken = record(3, "broken", session(0, 0), 0);
        broken.session = None;
        broken.body = None;
        let report = BrowserTestRunReport {
            tests: vec![
                record(0, "fast", session(800, 700), 100),
                slow,
                record(2, "waiting", session(700, 600), 300),
                broken,
            ],
            ..BrowserTestRunReport::default()
        };

        assert_that!(report.sessions_created()).is_equal_to(3);
        assert_that!(report.session_creation_time()).is_equal_to(Duration::from_millis(1800));
        assert_that!(report.session_wait_time()).is_equal_to(Duration::from_millis(1300));
        assert_that!(report.session_teardown_time()).is_equal_to(Duration::from_millis(30));
        assert_that!(report.body_time()).is_equal_to(Duration::from_millis(5400));
        let slowest: Vec<_> = report
            .slowest_tests()
            .into_iter()
            .map(|test| test.name.as_str())
            .collect();
        assert_that!(slowest).is_equal_to(vec!["slow", "waiting", "fast", "broken"]);
    }

    #[test]
    fn report_tells_created_from_reset_sessions() {
        let mut quit = record(2, "quit", reset(40, 0), 200);
        quit.teardown = Some(Duration::from_millis(30));
        let report = BrowserTestRunReport {
            total: Duration::from_secs(1),
            tests: vec![
                record(0, "first", session(800, 0), 100),
                record(1, "second", reset(60, 10), 300),
                quit,
            ],
            ..BrowserTestRunReport::default()
        };

        assert_that!(report.sessions_created()).is_equal_to(1);
        assert_that!(report.session_creation_time()).is_equal_to(Duration::from_millis(800));
        assert_that!(report.session_resets()).is_equal_to(2);
        assert_that!(report.session_reset_time()).is_equal_to(Duration::from_millis(100));
        let summary = report.to_string();
        assert_that!(summary.as_str()).contains(
            "1 created in 800ms (avg 800ms), reset 2x in 100ms (avg 50ms), tests waited 10ms for them, quit in 30ms",
        );
        assert_that!(summary.as_str()).contains("second (reset 60ms, waited 10ms, body 300ms)");
    }

    #[test]
    fn report_aggregates_steps_across_tests() {
        let mut first = record(0, "first", session(1, 0), 1);
        first.steps.insert(
            "goto".to_owned(),
            StepStats {
                count: 2,
                total: Duration::from_millis(300),
                max: Duration::from_millis(200),
            },
        );
        let mut second = record(1, "second", session(1, 0), 1);
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
        let mut failed = record(1, "broken", session(400, 15), 2500);
        failed.outcome = TestOutcome::Failed;
        failed.teardown = Some(Duration::from_millis(40));
        let report = BrowserTestRunReport {
            total: Duration::from_secs(5),
            webdriver_startup: Duration::from_millis(300),
            webdriver_shutdown: Duration::from_millis(50),
            tests: vec![record(0, "works", session(800, 0), 1000), failed],
            groups: vec![GroupRecord {
                name: "after all".to_owned(),
                duration: Duration::from_millis(1200),
            }],
        };

        let summary = report.to_string();

        assert_that!(summary.as_str())
            .contains("Browser test run: 2 test(s), 1 passed, 1 failed, in 5.00s");
        assert_that!(summary.as_str())
            .contains("2 created in 1.20s (avg 600ms), tests waited 15ms for them, quit in 40ms");
        assert_that!(summary.as_str())
            .contains("broken [failed] (session 400ms, waited 15ms, body 2.50s, quit 40ms)");
        assert_that!(summary.as_str()).contains("works (session 800ms, body 1.00s)");
        assert_that!(summary.as_str()).contains("  groups:\n        1.20s  after all");
        let broken = summary.find("broken").expect("summary lists the slow test");
        let works = summary.find("works").expect("summary lists the fast test");
        assert_that!(broken < works).is_true();
    }
}

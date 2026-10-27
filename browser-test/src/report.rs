//! Timing report of a browser test run.

use std::{
    collections::BTreeMap,
    fmt::{self, Display, Write as _},
    time::Duration,
};

/// The outcome and timing of every test of a browser test run.
///
/// Handed to the [`crate::RunReportConsumer`]s of the runner at the end of every run, and returned
/// by [`crate::BrowserTestRunner::run_with_report`]. Its `Display` implementation renders a
/// human-readable summary, as printed by [`crate::StderrSummary`].
///
/// The report types are `#[non_exhaustive]`, so that they can gain fields. To build reports, e.g.
/// to test a [`crate::RunReportConsumer`], start from [`RunReport::default`], [`TestRecord::new`],
/// [`SuiteRecord::new`], [`SessionTiming::new`] and [`StepStats::default`], and set their public
/// fields.
///
/// With the `serde` feature, the report types implement `serde::Serialize`, e.g. to write the
/// report as JSON. Durations serialize as seconds, a floating-point number (`1.25`), and enums in
/// `snake_case`: [`TestOutcome::TimedOut`] as `"timed_out"`, [`SessionPreparation::Created`] as
/// `{"created": 0.81}`. Only the fields serialize, not the values of methods like
/// [`Self::passed`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RunReport {
    /// Wall time of the whole run, from the start of [`crate::BrowserTestRunner::run`] until it
    /// returned. Includes the manual pause, if one was enabled.
    #[cfg_attr(feature = "serde", serde(with = "secs"))]
    pub total: Duration,

    /// Time spent resolving Chrome for Testing and starting chromedriver.
    #[cfg_attr(feature = "serde", serde(with = "secs"))]
    pub webdriver_startup: Duration,

    /// Time spent terminating chromedriver.
    #[cfg_attr(feature = "serde", serde(with = "secs"))]
    pub webdriver_shutdown: Duration,

    /// Every test that was started (or failed before it could start), in the order the tests were
    /// given to the runner. Tests not started because of [`crate::FailurePolicy::FailFast`] or a
    /// cancellation are missing, and counted in [`Self::not_started`].
    pub tests: Vec<TestRecord>,

    /// How many of the tests given to the runner did not start, because an earlier one failed
    /// ([`crate::FailurePolicy::FailFast`]) or the run was cancelled.
    pub not_started: usize,

    /// Every named suite (see [`crate::TestSuite::named`]) that ran, in the order the suites
    /// were defined.
    pub suites: Vec<SuiteRecord>,

    /// Time spent quitting sessions without a test: spares no test took, and, with
    /// [`crate::SessionReuse`], sessions the pool released after resetting them, or whose reset
    /// failed. The other quits are the [`TestRecord::teardown`] of a session's last test.
    #[cfg_attr(feature = "serde", serde(with = "secs"))]
    pub released_session_teardown: Duration,
}

/// Timing of one named suite of tests.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct SuiteRecord {
    /// The suite's name, prefixed with the names of enclosing named suites (`outer / inner`).
    pub name: String,

    /// Wall time from the suite's start until its last test finished.
    #[cfg_attr(feature = "serde", serde(with = "secs"))]
    pub duration: Duration,
}

impl SuiteRecord {
    /// The record of the suite `name` that ran for `duration`.
    #[must_use]
    pub fn new(name: impl Into<String>, duration: Duration) -> Self {
        Self {
            name: name.into(),
            duration,
        }
    }
}

/// Timing and outcome of one browser test.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct TestRecord {
    /// Position of the test in the collection given to the runner.
    pub index: usize,

    /// The test's name.
    pub name: String,

    /// How the test ended: passed, failed, panicked, timed out or cancelled.
    pub outcome: TestOutcome,

    /// Why the test failed: its failure report (see [`crate::failure_report`]), rendered as in the
    /// run's error, e.g. for a `JUnit` or JSON report. `None` if the test passed or was cancelled.
    pub failure: Option<String>,

    /// The names of the named [`crate::TestSuite`]s the test runs in, outermost first, joined
    /// with ` / `, as in the [`SuiteRecord::name`] of the innermost one. `None` if it runs in no
    /// named suite.
    pub path: Option<String>,

    /// The test's tags (see [`crate::BrowserTest::tags`]), which
    /// [`TestFilter::with_tag`](crate::TestFilter::with_tag) selects it by.
    pub tags: Vec<String>,

    /// How the test's session was prepared (created, or reset after an earlier test), and how long
    /// the test waited for it. `None` if no session was requested (e.g. one of the test's metadata
    /// methods panicked).
    pub session: Option<SessionTiming>,

    /// Time spent in [`crate::BrowserTest::run`]. `None` if the body never ran (e.g. the session
    /// could not be created).
    #[cfg_attr(feature = "serde", serde(with = "secs::option"))]
    pub body: Option<Duration>,

    /// Time spent quitting the session after the test. Quitting runs in the background, while the
    /// next test already runs. `None` if the session was kept to run another test (see
    /// [`crate::SessionReuse`], where resetting it counts towards that test's
    /// [`SessionPreparation::Reset`]), or if it could not be created or set up.
    #[cfg_attr(feature = "serde", serde(with = "secs::option"))]
    pub teardown: Option<Duration>,

    /// Time spent in [`crate::Step`]s of this test, per step kind.
    pub steps: BTreeMap<String, StepStats>,
}

impl TestRecord {
    /// The record of the test `name` at `index` that ended with `outcome`, with no failure, path,
    /// tags, session, body, teardown or steps.
    #[must_use]
    pub fn new(index: usize, name: impl Into<String>, outcome: TestOutcome) -> Self {
        Self {
            index,
            name: name.into(),
            outcome,
            failure: None,
            path: None,
            tags: Vec::new(),
            session: None,
            body: None,
            teardown: None,
            steps: BTreeMap::new(),
        }
    }

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
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum TestOutcome {
    /// The test passed.
    Passed,

    /// The test (or its session) failed.
    Failed,

    /// The test panicked.
    Panicked,

    /// The test ran longer than its time limit.
    TimedOut,

    /// The test was running when the run was cancelled. Its steps are not recorded.
    Cancelled,
}

impl TestOutcome {
    /// Whether the test passed.
    #[must_use]
    pub const fn is_passed(self) -> bool {
        matches!(self, Self::Passed)
    }

    /// Whether the test failed, panicked or timed out.
    #[must_use]
    pub const fn is_failed(self) -> bool {
        matches!(self, Self::Failed | Self::Panicked | Self::TimedOut)
    }

    /// Whether the test was cancelled while it ran.
    #[must_use]
    pub const fn is_cancelled(self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Timing of the `WebDriver` session of one test.
///
/// The runner prepares sessions ahead of the tests that use them (see
/// [`crate::BrowserTestRunner::with_spare_sessions`]), so preparing them usually overlaps earlier
/// tests and only [`Self::wait`] delays the test itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct SessionTiming {
    /// How the session was prepared for the test, and how long that took.
    pub preparation: SessionPreparation,

    /// Time the test waited for its session once it was its turn to run. Zero if the session was
    /// ready in time.
    #[cfg_attr(feature = "serde", serde(with = "secs"))]
    pub wait: Duration,
}

impl SessionTiming {
    /// The timing of a session prepared as `preparation`, which its test waited `wait` for.
    #[must_use]
    pub const fn new(preparation: SessionPreparation, wait: Duration) -> Self {
        Self { preparation, wait }
    }
}

/// How the session of a test was prepared, and how long that took (successful or not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum SessionPreparation {
    /// The session was created: a new browser started, and its page brought to the front.
    Created(#[cfg_attr(feature = "serde", serde(with = "secs"))] Duration),

    /// The session ran earlier tests and was reset after the last of them (see
    /// [`crate::SessionReuse`]).
    Reset(#[cfg_attr(feature = "serde", serde(with = "secs"))] Duration),
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
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct StepStats {
    /// How many steps of this kind ran.
    pub count: usize,

    /// Their summed duration. Nested steps count towards every enclosing kind as well.
    #[cfg_attr(feature = "serde", serde(with = "secs"))]
    pub total: Duration,

    /// The longest step of this kind.
    #[cfg_attr(feature = "serde", serde(with = "secs"))]
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

/// Serializes durations as seconds.
#[cfg(feature = "serde")]
mod secs {
    use std::time::Duration;

    use serde::Serializer;

    pub(super) fn serialize<S: Serializer>(
        duration: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(duration.as_secs_f64())
    }

    pub(super) mod option {
        use std::time::Duration;

        use serde::Serializer;

        #[expect(
            clippy::ref_option,
            reason = "serde's `with` passes a reference to the field"
        )]
        pub(in super::super) fn serialize<S: Serializer>(
            duration: &Option<Duration>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match duration {
                Some(duration) => serializer.serialize_some(&duration.as_secs_f64()),
                None => serializer.serialize_none(),
            }
        }
    }
}

/// How many entries the summary's lists show.
const SUMMARY_LIST_LEN: usize = 10;

/// The narrowest column of durations in the summary's lists, wide enough for durations below 100
/// minutes (`99m 59.9s`).
const MIN_DURATION_COLUMN_WIDTH: usize = 9;

impl RunReport {
    /// Number of tests that passed.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.tests
            .iter()
            .filter(|test| test.outcome.is_passed())
            .count()
    }

    /// Number of tests that failed, panicked or timed out.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.tests
            .iter()
            .filter(|test| test.outcome.is_failed())
            .count()
    }

    /// Number of tests that were running when the run was cancelled.
    #[must_use]
    pub fn cancelled(&self) -> usize {
        self.tests
            .iter()
            .filter(|test| test.outcome.is_cancelled())
            .count()
    }

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
        self.tests
            .iter()
            .filter_map(|test| test.teardown)
            .sum::<Duration>()
            + self.released_session_teardown
    }

    /// Total time spent in test bodies.
    #[must_use]
    pub fn body_time(&self) -> Duration {
        self.tests.iter().filter_map(|test| test.body).sum()
    }

    /// The tests sorted by [`TestRecord::total`], slowest first.
    #[must_use]
    pub fn slowest_tests(&self) -> Vec<&TestRecord> {
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
}

impl Display for RunReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Browser test run: {} test(s), {} passed, {} failed",
            self.tests.len() + self.not_started,
            self.passed(),
            self.failed(),
        )?;
        let cancelled = self.cancelled();
        if cancelled > 0 {
            write!(f, ", {cancelled} cancelled")?;
        }
        if self.not_started > 0 {
            write!(f, ", {} not started", self.not_started)?;
        }
        writeln!(f, ", in {}", FormatDuration(self.total))?;
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
        let slowest = self.slowest_tests();
        let slowest = &slowest[..slowest.len().min(SUMMARY_LIST_LEN)];
        let steps = self.slowest_steps();
        let steps = &steps[..steps.len().min(SUMMARY_LIST_LEN)];
        // The lists' durations share a column, right-aligned, as wide as the widest of them.
        let width = self
            .suites
            .iter()
            .map(|suite| suite.duration)
            .chain(slowest.iter().map(|test| test.total()))
            .chain(steps.iter().map(|(_, stats)| stats.total))
            .map(|duration| FormatDuration(duration).to_string().len())
            .fold(MIN_DURATION_COLUMN_WIDTH, usize::max);

        if !self.suites.is_empty() {
            writeln!(f, "  suites:")?;
            for suite in &self.suites {
                writeln!(
                    f,
                    "    {:>width$}  {}",
                    FormatDuration(suite.duration),
                    suite.name
                )?;
            }
        }

        if !slowest.is_empty() {
            writeln!(f, "  slowest tests:")?;
            for test in slowest {
                writeln!(
                    f,
                    "    {:>width$}  {}",
                    FormatDuration(test.total()),
                    Describe(test)
                )?;
            }
        }

        if !steps.is_empty() {
            writeln!(f, "  slowest steps (by total time):")?;
            for (kind, stats) in steps {
                writeln!(
                    f,
                    "    {:>width$}  {kind}: {}x{}, max {}",
                    FormatDuration(stats.total),
                    stats.count,
                    Average(stats.total, stats.count),
                    FormatDuration(stats.max),
                )?;
            }
        }
        Ok(())
    }
}

/// One test of the summary: name, outcome, and how its time splits up.
struct Describe<'a>(&'a TestRecord);

impl Display for Describe<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let test = self.0;
        f.write_str(&test.name)?;
        match test.outcome {
            TestOutcome::Passed => {}
            TestOutcome::Failed => f.write_str(" [failed]")?,
            TestOutcome::Panicked => f.write_str(" [panicked]")?,
            TestOutcome::TimedOut => f.write_str(" [timed out]")?,
            TestOutcome::Cancelled => f.write_str(" [cancelled]")?,
        }
        f.write_str(" (")?;
        f.write_str(&timing_breakdown(test))?;
        f.write_str(")")
    }
}

/// How a test's time splits up, e.g. `session 812ms, waited 120ms, body 3.20s, quit 40ms`, or
/// `reset 60ms, body 1.10s` in a reused session.
pub(crate) fn timing_breakdown(test: &TestRecord) -> String {
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
        // Rounded to the unit shown before choosing it, so that 999.6ms is `1.00s` and 59.996s is
        // `1m 00.0s`, and before splitting minutes, so that 119.96s is `2m 00.0s`, not `1m 60.0s`.
        let nanos = self.0.as_nanos();
        let millis = (nanos + 500_000) / 1_000_000;
        let hundredths = (nanos + 5_000_000) / 10_000_000;
        let text = if millis < 1000 {
            format!("{millis}ms")
        } else if hundredths < 6000 {
            format!("{}.{:02}s", hundredths / 100, hundredths % 100)
        } else {
            let tenths = (nanos + 50_000_000) / 100_000_000;
            let (minutes, tenths) = (tenths / 600, tenths % 600);
            format!("{minutes}m {:02}.{}s", tenths / 10, tenths % 10)
        };
        f.pad(&text)
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    fn record(index: usize, name: &str, session: SessionTiming, body_ms: u64) -> TestRecord {
        TestRecord {
            session: Some(session),
            body: Some(Duration::from_millis(body_ms)),
            ..TestRecord::new(index, name, TestOutcome::Passed)
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
        assert_that!(FormatDuration(Duration::from_millis(119_960)).to_string())
            .is_equal_to("2m 00.0s");
        // Rounded into the next unit at its edge.
        assert_that!(FormatDuration(Duration::from_micros(999_600)).to_string())
            .is_equal_to("1.00s");
        assert_that!(FormatDuration(Duration::from_micros(1_999_600)).to_string())
            .is_equal_to("2.00s");
        assert_that!(FormatDuration(Duration::from_millis(59_996)).to_string())
            .is_equal_to("1m 00.0s");
        assert_that!(FormatDuration(Duration::from_micros(1_600)).to_string()).is_equal_to("2ms");
    }

    #[test]
    fn report_aggregates_sessions_and_bodies() {
        let mut slow = record(1, "slow", session(300, 0), 5000);
        slow.teardown = Some(Duration::from_millis(30));
        let mut broken = record(3, "broken", session(0, 0), 0);
        broken.session = None;
        broken.body = None;
        let report = RunReport {
            tests: vec![
                record(0, "fast", session(800, 700), 100),
                slow,
                record(2, "waiting", session(700, 600), 300),
                broken,
            ],
            ..RunReport::default()
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
        let report = RunReport {
            total: Duration::from_secs(1),
            tests: vec![
                record(0, "first", session(800, 0), 100),
                record(1, "second", reset(60, 10), 300),
                quit,
            ],
            ..RunReport::default()
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
        let report = RunReport {
            tests: vec![first, second],
            ..RunReport::default()
        };

        let steps = report.slowest_steps();

        assert_that!(steps).has_length(2);
        assert_that!(steps[0].0.as_str()).is_equal_to("goto");
        assert_that!(steps[0].1).is_equal_to(StepStats {
            count: 3,
            total: Duration::from_millis(800),
            max: Duration::from_millis(500),
        });
        assert_that!(steps[1].0.as_str()).is_equal_to("wait");
    }

    #[test]
    fn cancelled_tests_count_apart_from_failed_ones() {
        let mut cancelled = record(1, "running", session(400, 15), 2500);
        cancelled.outcome = TestOutcome::Cancelled;
        let report = RunReport {
            tests: vec![record(0, "works", session(800, 0), 1000), cancelled],
            ..RunReport::default()
        };

        assert_that!((report.passed(), report.failed(), report.cancelled())).is_equal_to((1, 0, 1));
        let summary = report.to_string();
        assert_that!(summary.as_str()).contains("2 test(s), 1 passed, 0 failed, 1 cancelled, in");

        let report = RunReport {
            not_started: 3,
            ..report
        };
        assert_that!(report.to_string().as_str())
            .contains("5 test(s), 1 passed, 0 failed, 1 cancelled, 3 not started, in");
        assert_that!(summary.as_str()).contains("running [cancelled]");
    }

    #[test]
    fn summary_aligns_durations_of_100_minutes_and_more() {
        let mut long = record(0, "long", session(1, 0), 6_000_000);
        long.steps.insert(
            "wait".to_owned(),
            StepStats {
                count: 1,
                total: Duration::from_secs(6000),
                max: Duration::from_secs(6000),
            },
        );
        let report = RunReport {
            tests: vec![long, record(1, "short", session(1, 0), 500)],
            suites: vec![SuiteRecord::new("suite", Duration::from_secs(600))],
            ..RunReport::default()
        };

        let summary = report.to_string();

        assert_that!(summary.as_str()).contains("\n     10m 00.0s  suite\n");
        assert_that!(summary.as_str()).contains("\n    100m 00.0s  long (");
        assert_that!(summary.as_str()).contains("\n         500ms  short (");
        assert_that!(summary.as_str()).contains("\n    100m 00.0s  wait: 1x");
    }

    #[test]
    fn records_can_be_built_outside_the_runner() {
        let mut record = TestRecord::new(3, "login", TestOutcome::Failed);
        record.failure = Some("Browser test 'login' failed.".to_owned());
        record.session = Some(SessionTiming::new(
            SessionPreparation::Created(Duration::from_millis(800)),
            Duration::ZERO,
        ));

        assert_that!(record.index).is_equal_to(3);
        assert_that!(record.name.as_str()).is_equal_to("login");
        assert_that!(record.total()).is_equal_to(Duration::ZERO);
        assert_that!(record.steps).is_empty();
    }

    #[cfg(feature = "serde")]
    #[test]
    fn report_serializes_durations_as_seconds() {
        let mut failed = record(0, "login", reset(250, 0), 1500);
        failed.outcome = TestOutcome::TimedOut;
        failed.failure = Some("timed out".to_owned());
        failed.steps.insert(
            "goto".to_owned(),
            StepStats {
                count: 2,
                total: Duration::from_millis(500),
                max: Duration::from_millis(400),
            },
        );
        let report = RunReport {
            total: Duration::from_millis(2250),
            tests: vec![failed],
            ..RunReport::default()
        };

        let json = serde_json::to_value(&report).expect("the report serializes");

        assert_that!(json).is_equal_to(serde_json::json!({
            "total": 2.25,
            "webdriver_startup": 0.0,
            "webdriver_shutdown": 0.0,
            "tests": [{
                "index": 0,
                "name": "login",
                "outcome": "timed_out",
                "failure": "timed out",
                "path": null,
                "tags": [],
                "session": { "preparation": { "reset": 0.25 }, "wait": 0.0 },
                "body": 1.5,
                "teardown": null,
                "steps": { "goto": { "count": 2, "total": 0.5, "max": 0.4 } },
            }],
            "not_started": 0,
            "suites": [],
            "released_session_teardown": 0.0,
        }));
    }

    #[test]
    fn summary_lists_sessions_and_slowest_tests() {
        let mut failed = record(1, "broken", session(400, 15), 2500);
        failed.outcome = TestOutcome::Failed;
        failed.teardown = Some(Duration::from_millis(40));
        let report = RunReport {
            total: Duration::from_secs(5),
            webdriver_startup: Duration::from_millis(300),
            webdriver_shutdown: Duration::from_millis(50),
            tests: vec![record(0, "works", session(800, 0), 1000), failed],
            suites: vec![SuiteRecord {
                name: "after all".to_owned(),
                duration: Duration::from_millis(1200),
            }],
            released_session_teardown: Duration::from_millis(60),
            not_started: 0,
        };

        let summary = report.to_string();

        assert_that!(summary.as_str())
            .contains("Browser test run: 2 test(s), 1 passed, 1 failed, in 5.00s");
        assert_that!(summary.as_str())
            .contains("2 created in 1.20s (avg 600ms), tests waited 15ms for them, quit in 100ms");
        assert_that!(summary.as_str())
            .contains("broken [failed] (session 400ms, waited 15ms, body 2.50s, quit 40ms)");
        assert_that!(summary.as_str()).contains("works (session 800ms, body 1.00s)");
        assert_that!(summary.as_str()).contains("  suites:\n        1.20s  after all");
        let broken = summary.find("broken").expect("summary lists the slow test");
        let works = summary.find("works").expect("summary lists the fast test");
        assert_that!(broken).is_less_than(works);
    }
}

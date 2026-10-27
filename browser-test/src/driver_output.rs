use std::{
    collections::VecDeque,
    fmt,
    num::NonZeroUsize,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use chrome_for_testing_manager::{
    ChromeForTesting, DriverOutputLine, DriverOutputSource, DriverOutputSubscriptionError,
};
use rootcause::{
    Report,
    handlers::{
        AttachmentFormattingPlacement, AttachmentFormattingStyle, AttachmentHandler,
        FormattingFunction,
    },
    report_attachment::ReportAttachment,
};

use crate::{
    BrowserTestError,
    env::{InvalidEnvVar, env_flag, env_positive_number},
};

/// Default environment variable enabling browser driver output capture.
pub(crate) const DEFAULT_BROWSER_DRIVER_OUTPUT_ENV: &str = "BROWSER_TEST_DRIVER_OUTPUT";

/// Default number of browser driver output lines retained when env capture is enabled.
pub(crate) const DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES: NonZeroUsize =
    NonZeroUsize::new(200).expect("the default is non-zero");

/// Capture of recent browser-driver output, attached to the errors of failed tests and runs.
///
/// The report of a failed test shows the output while it ran and its session quit: chromedriver's
/// lines of the test's session and those of no session, without the lines of other sessions that
/// ran at the same time. The error of a run that failed otherwise, e.g. as a session couldn't be
/// created or chromedriver didn't shut down, shows the run's last lines. Both come from the
/// retained lines, so a test whose output outgrows them shows its last lines only.
///
/// Disabled by default.
///
/// # Examples
///
/// ```
/// use browser_test::{BrowserTestRunner, Cancellation, DriverOutput};
///
/// let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals())
///     .with_driver_output(DriverOutput::enabled().with_tail_lines(100));
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DriverOutput {
    tail_lines: Option<NonZeroUsize>,
}

impl DriverOutput {
    /// Do not capture browser-driver output.
    #[must_use]
    pub const fn disabled() -> Self {
        Self { tail_lines: None }
    }

    /// Capture the last 200 lines of browser-driver output. Choose how many with
    /// [`Self::with_tail_lines`].
    #[must_use]
    pub const fn enabled() -> Self {
        Self {
            tail_lines: Some(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES),
        }
    }

    /// Capture the last `tail_lines` lines instead of 200. Has no effect on a disabled capture.
    ///
    /// # Panics
    ///
    /// Panics if `tail_lines` is zero. Use [`Self::disabled`] to capture nothing.
    #[must_use]
    #[track_caller]
    pub const fn with_tail_lines(mut self, tail_lines: usize) -> Self {
        let Some(tail_lines) = NonZeroUsize::new(tail_lines) else {
            panic!(
                "capturing zero lines of driver output captures nothing, use `DriverOutput::disabled()`"
            );
        };
        if self.tail_lines.is_some() {
            self.tail_lines = Some(tail_lines);
        }
        self
    }

    /// Read the capture from `BROWSER_TEST_DRIVER_OUTPUT` and
    /// `BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES`.
    ///
    /// See [`Self::from_env_var`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if a variable is set to a value that cannot be interpreted.
    pub fn from_env() -> Result<Option<Self>, InvalidEnvVar> {
        Self::from_env_var(DEFAULT_BROWSER_DRIVER_OUTPUT_ENV)
    }

    /// Read the capture from the boolean flag `env_var` and the number of lines from
    /// `<env_var>_TAIL_LINES`.
    ///
    /// Returns `None` if `env_var` is unset or empty, so the caller picks the default:
    /// `DriverOutput::from_env()?.unwrap_or_default()`. `1`, `true`, `yes`, `on`, and `enabled`
    /// enable capture, `0`, `false`, `no`, `off`, and `disabled` disable it (ignoring case). An
    /// enabled capture retains `<env_var>_TAIL_LINES` lines, a positive number, or 200 if that
    /// variable is unset or empty. Both variables are read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if `env_var` is not a boolean flag or `<env_var>_TAIL_LINES` is
    /// not a positive number.
    pub fn from_env_var(env_var: impl AsRef<str>) -> Result<Option<Self>, InvalidEnvVar> {
        let env_var = env_var.as_ref();
        match env_flag(env_var)? {
            None => Ok(None),
            Some(false) => Ok(Some(Self::disabled())),
            Some(true) => {
                let tail_lines = env_positive_number(&format!("{env_var}_TAIL_LINES"))?
                    .unwrap_or(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES);
                Ok(Some(Self::enabled().with_tail_lines(tail_lines.get())))
            }
        }
    }

    /// Number of recent output lines retained, or `None` if capture is disabled. See
    /// [`Self::with_tail_lines`].
    #[must_use]
    pub const fn tail_lines(self) -> Option<NonZeroUsize> {
        self.tail_lines
    }
}

/// Shared capture handle for browser driver output.
#[derive(Debug, Clone)]
pub(crate) struct DriverOutputCapture {
    inner: Arc<Mutex<BrowserDriverOutputState>>,
}

#[derive(Debug)]
struct BrowserDriverOutputState {
    tail_capacity: NonZeroUsize,
    /// Sequence number of the next line: lines captured or skipped so far.
    next_sequence: usize,
    /// Lines captured so far, without skipped ones.
    captured_lines: usize,
    tail_lines: VecDeque<CapturedDriverOutputLine>,
    /// Lines skipped since the oldest retained line, oldest first.
    skipped: VecDeque<SkippedLines>,
}

/// A captured driver output line and its position in the captured output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapturedDriverOutputLine {
    /// Zero-based position of the line in the output seen by the capture.
    pub(crate) sequence: usize,

    /// The driver output line.
    pub(crate) line: DriverOutputLine,
}

/// Lines that were not captured because capturing fell behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SkippedLines {
    /// The position of the first of them in the output seen by the capture.
    pub(crate) sequence: usize,
    pub(crate) count: usize,
}

/// How long [`DriverOutputFollower::finish`] waits for output still in flight after the driver
/// was shut down.
const FINISH_FOLLOWING_TIMEOUT: Duration = Duration::from_secs(1);

/// How long [`DriverOutputCapture::catch_up`] waits. Copying a line takes microseconds, unless the
/// machine is busy.
const CATCH_UP_TIME: Duration = Duration::from_millis(20);

/// A task copying live driver output into a [`DriverOutputCapture`].
#[derive(Debug)]
pub(crate) struct DriverOutputFollower {
    task: tokio::task::JoinHandle<()>,
}

impl DriverOutputFollower {
    /// Wait briefly for the output of a terminated driver to be copied, then stop following.
    pub(crate) async fn finish(self) {
        let mut task = self.task;
        if tokio::time::timeout(FINISH_FOLLOWING_TIMEOUT, &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
}

impl DriverOutputCapture {
    /// Create a capture handle retaining the last `tail_lines` driver output lines.
    ///
    /// The tail grows with the output, rather than reserving `tail_lines` up front: a large value
    /// (`usize::MAX` to keep everything) costs nothing until the driver prints that much.
    #[must_use]
    pub(crate) fn new(tail_lines: NonZeroUsize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BrowserDriverOutputState {
                tail_capacity: tail_lines,
                next_sequence: 0,
                captured_lines: 0,
                tail_lines: VecDeque::new(),
                skipped: VecDeque::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BrowserDriverOutputState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sequence number of the next line, to mark where a test's output starts.
    #[must_use]
    pub(crate) fn position(&self) -> usize {
        self.lock().next_sequence
    }

    /// Give the task following the driver's output time to copy the lines the driver printed
    /// right before, e.g. those about the command that failed a test.
    pub(crate) async fn catch_up(&self) {
        tokio::time::sleep(CATCH_UP_TIME).await;
    }

    /// Return a snapshot of the currently captured output tail.
    #[must_use]
    pub(crate) fn snapshot(&self) -> DriverOutputSnapshot {
        let state = self.lock();
        DriverOutputSnapshot {
            tail_lines: state.tail_lines.iter().cloned().collect(),
            skipped: state.skipped.iter().copied().collect(),
            scope: OutputScope::Run {
                captured_lines: state.captured_lines,
            },
        }
    }

    /// The output since `start` (a [`Self::position`]) of the session `session_id`, and the lines
    /// of no session, e.g. the driver's own messages. `None` if there is none.
    #[must_use]
    pub(crate) fn session_output(
        &self,
        start: usize,
        session_id: &str,
    ) -> Option<DriverOutputSnapshot> {
        let state = self.lock();
        let mut tail_lines = Vec::new();
        let mut other_sessions = 0;
        for (captured, session) in state
            .tail_lines
            .iter()
            .zip(line_sessions(&state.tail_lines))
        {
            if captured.sequence < start {
                continue;
            }
            if session.is_some_and(|session| session != session_id) {
                other_sessions += 1;
            } else {
                tail_lines.push(captured.clone());
            }
        }
        let skipped: Vec<_> = state
            .skipped
            .iter()
            .filter(|skipped| skipped.sequence + skipped.count > start)
            .copied()
            .collect();
        if tail_lines.is_empty() && skipped.is_empty() {
            return None;
        }
        let earlier_lines_lost = state
            .tail_lines
            .front()
            .is_some_and(|oldest| oldest.sequence > start);
        Some(DriverOutputSnapshot {
            tail_lines,
            skipped,
            scope: OutputScope::Session {
                other_sessions,
                earlier_lines_lost,
            },
        })
    }

    /// Capture the driver's output so far, then keep capturing its live output until the driver
    /// shuts down. Every line is captured once.
    pub(crate) fn follow(&self, chrome: &ChromeForTesting) -> DriverOutputFollower {
        let (history, mut subscription) = chrome.subscribe_output_with_history();
        for line in history {
            self.push(line);
        }
        let capture = self.clone();
        let task = tokio::spawn(async move {
            loop {
                match subscription.recv().await {
                    Ok(line) => capture.push(line),
                    Err(DriverOutputSubscriptionError::Lagged { skipped }) => {
                        capture.skip(skipped);
                    }
                    // `Closed`, or a future error: no more output can be received.
                    Err(_) => return,
                }
            }
        });
        DriverOutputFollower { task }
    }

    pub(crate) fn push(&self, line: DriverOutputLine) {
        let mut state = self.lock();
        let sequence = state.next_sequence;
        state.next_sequence += 1;
        state.captured_lines += 1;

        while state.tail_lines.len() >= state.tail_capacity.get() {
            state.tail_lines.pop_front();
        }
        let oldest = state
            .tail_lines
            .front()
            .map_or(sequence, |oldest| oldest.sequence);
        while state
            .skipped
            .front()
            .is_some_and(|skipped| skipped.sequence < oldest)
        {
            state.skipped.pop_front();
        }
        state
            .tail_lines
            .push_back(CapturedDriverOutputLine { sequence, line });
    }

    /// Account for `count` lines that were not captured because capturing fell behind.
    fn skip(&self, count: u64) {
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        if count == 0 {
            return;
        }
        let mut state = self.lock();
        let sequence = state.next_sequence;
        state.next_sequence = sequence.saturating_add(count);
        state.skipped.push_back(SkippedLines { sequence, count });
    }
}

/// The session each of `lines` belongs to, by chromedriver's log format: an entry starts with
/// `[<timestamp>][<level>]: `, followed by `[<session id>] ` if a session logged it. A line that
/// starts no entry continues the last entry of its stream, e.g. the JSON of a command.
fn line_sessions<'l>(
    lines: impl IntoIterator<Item = &'l CapturedDriverOutputLine>,
) -> Vec<Option<&'l str>> {
    let mut current: [Option<&str>; 2] = [None, None];
    lines
        .into_iter()
        .map(|captured| {
            let stream = match captured.line.source {
                DriverOutputSource::Stdout => 0,
                DriverOutputSource::Stderr => 1,
            };
            let line = captured.line.line.as_str();
            if let Some((_, message)) = line.split_once("]: ").filter(|_| {
                line.starts_with('[') && line[1..].starts_with(|c: char| c.is_ascii_digit())
            }) {
                current[stream] = message
                    .strip_prefix('[')
                    .and_then(|rest| rest.split_once(']'))
                    .map(|(id, _)| id)
                    .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()));
            }
            current[stream]
        })
        .collect()
}

/// Snapshot of browser driver output captured so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriverOutputSnapshot {
    /// Recent output lines in capture order.
    pub(crate) tail_lines: Vec<CapturedDriverOutputLine>,

    /// Lines skipped between and after them, oldest first.
    pub(crate) skipped: Vec<SkippedLines>,

    pub(crate) scope: OutputScope,
}

/// What a [`DriverOutputSnapshot`] covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputScope {
    /// The whole run, of which `captured_lines` were captured.
    Run { captured_lines: usize },
    /// The output while a test ran, of its session and of no session. `other_sessions` lines of
    /// other sessions were left out. Lines before the oldest retained one are lost if
    /// `earlier_lines_lost`.
    Session {
        other_sessions: usize,
        earlier_lines_lost: bool,
    },
}

impl DriverOutputSnapshot {
    /// Whether the retained output tail is empty.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.tail_lines.is_empty() && self.skipped.is_empty()
    }
}

#[derive(Debug, Clone)]
struct DriverOutputAttachment {
    snapshot: DriverOutputSnapshot,
}

struct DriverOutputAttachmentHandler;

impl AttachmentHandler<DriverOutputAttachment> for DriverOutputAttachmentHandler {
    fn display(value: &DriverOutputAttachment, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = &value.snapshot;
        let shown = snapshot.tail_lines.len();
        match snapshot.scope {
            OutputScope::Run { captured_lines } => {
                writeln!(formatter, "Recent browser driver output")?;
                writeln!(
                    formatter,
                    "note: output comes from one shared browser-driver process; parallel tests may interleave lines."
                )?;
                if captured_lines > shown {
                    writeln!(
                        formatter,
                        "showing the last {shown} of {captured_lines} captured line(s)."
                    )?;
                }
            }
            OutputScope::Session {
                other_sessions,
                earlier_lines_lost,
            } => {
                writeln!(formatter, "Browser driver output while the test ran")?;
                writeln!(
                    formatter,
                    "note: lines of the test's session and of no session; {other_sessions} line(s) of other sessions are left out."
                )?;
                if earlier_lines_lost {
                    writeln!(
                        formatter,
                        "showing the last {shown} line(s): earlier ones are no longer retained."
                    )?;
                }
            }
        }

        let mut skipped = snapshot.skipped.iter().peekable();
        for captured in &snapshot.tail_lines {
            while let Some(gap) = skipped.next_if(|gap| gap.sequence < captured.sequence) {
                write_skipped(formatter, gap)?;
            }
            writeln!(
                formatter,
                "[{} {}] {}",
                captured.sequence,
                source_label(captured.line.source),
                captured.line.line,
            )?;
        }
        for gap in skipped {
            write_skipped(formatter, gap)?;
        }

        Ok(())
    }

    fn debug(value: &DriverOutputAttachment, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Self::display(value, formatter)
    }

    fn preferred_formatting_style(
        value: &DriverOutputAttachment,
        _report_formatting: FormattingFunction,
    ) -> AttachmentFormattingStyle {
        let appendix_name = match value.snapshot.scope {
            OutputScope::Run { .. } => "Recent browser driver output",
            OutputScope::Session { .. } => "Browser driver output of the test",
        };
        AttachmentFormattingStyle {
            placement: AttachmentFormattingPlacement::Appendix { appendix_name },
            function: FormattingFunction::Display,
            priority: 5,
        }
    }
}

fn write_skipped(formatter: &mut fmt::Formatter<'_>, skipped: &SkippedLines) -> fmt::Result {
    writeln!(
        formatter,
        "[{} ...] ({} line(s) skipped, as capturing fell behind)",
        skipped.sequence, skipped.count,
    )
}

pub(crate) fn attach_browser_driver_output(
    report: &mut Report<BrowserTestError>,
    capture: Option<&DriverOutputCapture>,
) {
    let Some(snapshot) = capture.map(DriverOutputCapture::snapshot) else {
        return;
    };
    attach_snapshot(report, snapshot);
}

/// Attach the output of a test's session, see [`DriverOutputCapture::session_output`].
pub(crate) fn attach_session_output(
    mut report: Report<BrowserTestError>,
    output: Option<DriverOutputSnapshot>,
) -> Report<BrowserTestError> {
    if let Some(snapshot) = output {
        attach_snapshot(&mut report, snapshot);
    }
    report
}

fn attach_snapshot(report: &mut Report<BrowserTestError>, snapshot: DriverOutputSnapshot) {
    if snapshot.is_empty() {
        return;
    }
    report.attachments_mut().push(
        ReportAttachment::new_custom::<DriverOutputAttachmentHandler>(DriverOutputAttachment {
            snapshot,
        })
        .into_dynamic(),
    );
}

/// Whether every failed test of the failed run `report` (a [`BrowserTestError::RunTests`]) carries
/// the output of its session, so that the run's output adds nothing.
pub(crate) fn every_failure_has_session_output(report: &Report<BrowserTestError>) -> bool {
    !report.children().is_empty()
        && report.children().iter().all(|failure| {
            failure.attachments().iter().any(|attachment| {
                attachment
                    .downcast_inner::<DriverOutputAttachment>()
                    .is_some_and(|output| {
                        matches!(output.snapshot.scope, OutputScope::Session { .. })
                    })
            })
        })
}

pub(crate) fn attach_browser_driver_output_to_result(
    result: Result<(), Report<BrowserTestError>>,
    capture: Option<&DriverOutputCapture>,
) -> Result<(), Report<BrowserTestError>> {
    result.map_err(|mut err| {
        attach_browser_driver_output(&mut err, capture);
        err
    })
}

fn source_label(source: DriverOutputSource) -> &'static str {
    match source {
        DriverOutputSource::Stdout => "stdout",
        DriverOutputSource::Stderr => "stderr",
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;
    use crate::test_support::EnvVarGuard;

    #[test]
    #[should_panic(expected = "captures nothing")]
    fn tail_lines_rejects_zero() {
        let _ = DriverOutput::enabled().with_tail_lines(0);
    }

    #[test]
    fn tail_lines_apply_to_enabled_captures() {
        assert_that!(DriverOutput::default()).is_equal_to(DriverOutput::disabled());
        assert_that!(DriverOutput::enabled().tail_lines())
            .is_equal_to(Some(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES));
        assert_that!(
            DriverOutput::enabled()
                .with_tail_lines(3)
                .tail_lines()
                .map(NonZeroUsize::get)
        )
        .is_equal_to(Some(3));
        assert_that!(DriverOutput::disabled().with_tail_lines(3).tail_lines()).is_none();
    }

    #[test]
    fn from_env_reads_flag_and_tail_lines() {
        let env = EnvVarGuard::new(DEFAULT_BROWSER_DRIVER_OUTPUT_ENV);
        let tail_lines = env.also("BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES");

        env.set("1");
        tail_lines.set("12");
        assert_that!(DriverOutput::from_env())
            .is_equal_to(Ok(Some(DriverOutput::enabled().with_tail_lines(12))));

        tail_lines.remove();
        assert_that!(DriverOutput::from_env()).is_equal_to(Ok(Some(DriverOutput::enabled())));

        tail_lines.set("lots");
        assert_that!(DriverOutput::from_env()).is_err();
        tail_lines.set("0");
        assert_that!(DriverOutput::from_env()).is_err();

        env.set("0");
        assert_that!(DriverOutput::from_env()).is_equal_to(Ok(Some(DriverOutput::disabled())));

        env.remove();
        assert_that!(DriverOutput::from_env()).is_equal_to(Ok(None));
    }

    #[test]
    fn capture_retains_bounded_tail_and_total_count() {
        let capture = DriverOutputCapture::new(
            NonZeroUsize::new(2).expect("literal tail capacity should be non-zero"),
        );

        capture.push(line(DriverOutputSource::Stdout, "one"));
        capture.push(line(DriverOutputSource::Stderr, "two"));
        capture.skip(2);
        capture.push(line(DriverOutputSource::Stdout, "three"));

        let snapshot = capture.snapshot();

        assert_that!(capture.position()).is_equal_to(5);
        assert_that!(snapshot.scope.clone()).is_equal_to(OutputScope::Run { captured_lines: 3 });
        assert_that!(snapshot.tail_lines).has_length(2);
        assert_that!(&snapshot.tail_lines[0].line.line).is_equal_to("two");
        assert_that!(snapshot.tail_lines[0].sequence).is_equal_to(1);
        assert_that!(&snapshot.tail_lines[1].line.line).is_equal_to("three");
        assert_that!(snapshot.tail_lines[1].sequence).is_equal_to(4);
        assert_that!(snapshot.tail_lines[0].line.source).is_equal_to(DriverOutputSource::Stderr);
        assert_that!(snapshot.skipped.clone()).is_equal_to(vec![SkippedLines {
            sequence: 2,
            count: 2,
        }]);
        assert_that!(output_of(snapshot)).is_equal_to(
            "Recent browser driver output\nnote: output comes from one shared browser-driver \
             process; parallel tests may interleave lines.\nshowing the last 2 of 3 captured \
             line(s).\n[1 stderr] two\n[2 ...] (2 line(s) skipped, as capturing fell behind)\n\
             [4 stdout] three\n"
                .to_owned(),
        );
    }

    #[test]
    fn skipped_lines_are_forgotten_with_the_lines_before_them() {
        let capture = DriverOutputCapture::new(
            NonZeroUsize::new(2).expect("literal tail capacity should be non-zero"),
        );
        capture.push(line(DriverOutputSource::Stdout, "one"));
        capture.skip(3);
        capture.push(line(DriverOutputSource::Stdout, "two"));
        capture.push(line(DriverOutputSource::Stdout, "three"));

        assert_that!(capture.snapshot().skipped).is_empty();
    }

    #[test]
    fn session_output_keeps_the_lines_of_the_session_and_of_no_session() {
        let session = "0123456789abcdef0123456789abcdef";
        let other = "fedcba9876543210fedcba9876543210";
        let capture = DriverOutputCapture::new(
            NonZeroUsize::new(20).expect("literal tail capacity should be non-zero"),
        );
        let stderr = |text: &str| capture.push(line(DriverOutputSource::Stderr, text));
        stderr(&format!(
            "[1730000000.001][INFO]: [{session}] COMMAND GetTitle {{"
        ));
        stderr("}");
        let start = capture.position();
        stderr(&format!(
            "[1730000000.002][INFO]: [{session}] COMMAND FindElement {{"
        ));
        stderr(r#"   "using": "css selector","#);
        stderr(&format!(
            "[1730000000.003][INFO]: [{other}] COMMAND GetTitle {{"
        ));
        stderr("}");
        capture.skip(2);
        stderr("[1730000000.004][INFO]: Waiting for pending navigations...");
        stderr(&format!(
            "[1730000000.005][INFO]: [{session}] RESPONSE FindElement ERROR no such element"
        ));
        capture.push(line(
            DriverOutputSource::Stdout,
            "ChromeDriver was started successfully.",
        ));

        let output = capture
            .session_output(start, session)
            .expect("the session printed lines");

        assert_that!(output.scope.clone()).is_equal_to(OutputScope::Session {
            other_sessions: 2,
            earlier_lines_lost: false,
        });
        assert_that!(output_of(output)).is_equal_to(format!(
            "Browser driver output while the test ran\nnote: lines of the test's session and of \
             no session; 2 line(s) of other sessions are left out.\n\
             [2 stderr] [1730000000.002][INFO]: [{session}] COMMAND FindElement {{\n\
             [3 stderr]    \"using\": \"css selector\",\n\
             [6 ...] (2 line(s) skipped, as capturing fell behind)\n\
             [8 stderr] [1730000000.004][INFO]: Waiting for pending navigations...\n\
             [9 stderr] [1730000000.005][INFO]: [{session}] RESPONSE FindElement ERROR no such \
             element\n\
             [10 stdout] ChromeDriver was started successfully.\n"
        ));
        assert_that!(capture.session_output(capture.position(), session)).is_none();
    }

    #[test]
    fn session_output_says_when_earlier_lines_are_lost() {
        let capture = DriverOutputCapture::new(
            NonZeroUsize::new(1).expect("literal tail capacity should be non-zero"),
        );
        let start = capture.position();
        capture.push(line(DriverOutputSource::Stderr, "one"));
        capture.push(line(DriverOutputSource::Stderr, "two"));

        let output = capture
            .session_output(start, "0123")
            .expect("lines were printed");

        assert_that!(output_of(output))
            .contains("showing the last 1 line(s): earlier ones are no longer retained.");
    }

    #[test]
    fn a_failed_run_needs_its_output_unless_every_failure_has_its_own() {
        let capture = DriverOutputCapture::new(
            NonZeroUsize::new(5).expect("literal tail capacity should be non-zero"),
        );
        capture.push(line(
            DriverOutputSource::Stderr,
            "[1730000000.001][INFO]: [0123] COMMAND",
        ));
        let failure = |with_output: bool| {
            let report = Report::new(BrowserTestError::RunTest {
                test_name: "login".to_owned(),
            });
            let output = with_output
                .then(|| capture.session_output(0, "0123"))
                .flatten();
            attach_session_output(report, output).into_cloneable()
        };
        let run = |failures: Vec<_>| {
            let mut collection = rootcause::report_collection::ReportCollection::new();
            for failure in failures {
                collection.push(failure);
            }
            collection.context(BrowserTestError::RunTests { failed_tests: 2 })
        };

        assert_that!(every_failure_has_session_output(&run(vec![
            failure(true),
            failure(true)
        ])))
        .is_true();
        assert_that!(every_failure_has_session_output(&run(vec![
            failure(true),
            failure(false)
        ])))
        .is_false();
        assert_that!(every_failure_has_session_output(&Report::new(
            BrowserTestError::ShutDownChromeForTesting
        )))
        .is_false();
    }

    fn output_of(snapshot: DriverOutputSnapshot) -> String {
        struct Output(DriverOutputAttachment);
        impl fmt::Display for Output {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                DriverOutputAttachmentHandler::display(&self.0, f)
            }
        }
        Output(DriverOutputAttachment { snapshot }).to_string()
    }

    #[test]
    fn capture_of_unbounded_tail_reserves_nothing_up_front() {
        let capture = DriverOutputCapture::new(NonZeroUsize::MAX);

        capture.push(line(DriverOutputSource::Stdout, "one"));

        assert_that!(capture.snapshot().tail_lines).has_length(1);
    }

    #[test]
    fn browser_driver_output_is_rootcause_attachment_not_child_error() {
        let capture = DriverOutputCapture::new(
            NonZeroUsize::new(5).expect("literal tail capacity should be non-zero"),
        );
        capture.push(line(DriverOutputSource::Stdout, "Starting ChromeDriver"));
        let mut report: Report<BrowserTestError> = Report::new(BrowserTestError::RunTest {
            test_name: "login".to_owned(),
        });
        let initial_attachment_count = report.attachments().len();

        attach_browser_driver_output(&mut report, Some(&capture));

        assert_that!(report).has_attachment_count(initial_attachment_count + 1);
        assert_that!(report).has_child_count(0);
        assert_that!(report.to_string()).contains("Recent browser driver output");
        assert_that!(report.to_string()).contains("parallel tests may interleave lines");
    }

    fn line(source: DriverOutputSource, line: &str) -> DriverOutputLine {
        DriverOutputLine::new(source, line)
    }
}

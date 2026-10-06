use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrome_for_testing_manager::{
    ChromeForTesting, DriverOutputLine, DriverOutputSource, DriverOutputSubscriptionError,
};
use rootcause::Report;
use rootcause::handlers::{
    AttachmentFormattingPlacement, AttachmentFormattingStyle, AttachmentHandler, FormattingFunction,
};
use rootcause::report_attachment::ReportAttachment;

use crate::BrowserTestError;
use crate::env::{InvalidEnvVar, env_flag, env_number};

/// Default environment variable enabling browser driver output capture.
pub(crate) const DEFAULT_BROWSER_DRIVER_OUTPUT_ENV: &str = "BROWSER_TEST_DRIVER_OUTPUT";

/// Default number of browser driver output lines retained when env capture is enabled.
pub(crate) const DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES: usize = 200;

/// Capture of recent browser-driver output, attached to the errors of failed runs and tests.
///
/// Disabled by default.
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

    /// Capture the last `tail_lines` browser-driver output lines. `0` disables capture.
    #[must_use]
    pub const fn tail_lines(tail_lines: usize) -> Self {
        Self {
            tail_lines: NonZeroUsize::new(tail_lines),
        }
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
    /// enabled capture retains `<env_var>_TAIL_LINES` lines, or 200 if that variable is unset or
    /// empty. `0` lines disable capture. Both variables are read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if `env_var` is not a boolean flag or `<env_var>_TAIL_LINES` is
    /// not a number.
    pub fn from_env_var(env_var: impl AsRef<str>) -> Result<Option<Self>, InvalidEnvVar> {
        let env_var = env_var.as_ref();
        match env_flag(env_var)? {
            None => Ok(None),
            Some(false) => Ok(Some(Self::disabled())),
            Some(true) => {
                let tail_lines = env_number(&format!("{env_var}_TAIL_LINES"))?
                    .unwrap_or(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES);
                Ok(Some(Self::tail_lines(tail_lines)))
            }
        }
    }

    /// Number of recent output lines retained, or `None` if capture is disabled.
    #[must_use]
    pub const fn tail_line_count(self) -> Option<NonZeroUsize> {
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
    total_lines: usize,
    tail_lines: VecDeque<CapturedDriverOutputLine>,
}

/// A captured driver output line and its position in the captured output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapturedDriverOutputLine {
    /// Zero-based position of the line in the output seen by the capture.
    pub(crate) sequence: usize,

    /// The driver output line.
    pub(crate) line: DriverOutputLine,
}

/// How long [`DriverOutputFollower::finish`] waits for output still in flight after the driver
/// was shut down.
const FINISH_FOLLOWING_TIMEOUT: Duration = Duration::from_secs(1);

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
    #[must_use]
    pub(crate) fn new(tail_lines: NonZeroUsize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BrowserDriverOutputState {
                tail_capacity: tail_lines,
                total_lines: 0,
                tail_lines: VecDeque::with_capacity(tail_lines.get()),
            })),
        }
    }

    /// Return a snapshot of the currently captured output tail.
    ///
    /// # Panics
    ///
    /// Panics if the internal output capture mutex has been poisoned.
    #[must_use]
    pub(crate) fn snapshot(&self) -> DriverOutputSnapshot {
        let state = self
            .inner
            .lock()
            .expect("browser driver output capture mutex should not be poisoned");
        DriverOutputSnapshot {
            total_lines: state.total_lines,
            tail_capacity: state.tail_capacity.get(),
            tail_lines: state.tail_lines.iter().cloned().collect(),
        }
    }

    /// Capture the driver's output so far, then keep capturing its live output until the driver
    /// shuts down.
    ///
    /// A line printed while following starts can be captured twice.
    pub(crate) fn follow(&self, chrome: &ChromeForTesting) -> DriverOutputFollower {
        let mut subscription = chrome.subscribe_output();
        for line in chrome.recent_output() {
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
        let mut state = self
            .inner
            .lock()
            .expect("browser driver output capture mutex should not be poisoned");
        let sequence = state.total_lines;
        state.total_lines += 1;

        while state.tail_lines.len() >= state.tail_capacity.get() {
            state.tail_lines.pop_front();
        }
        state
            .tail_lines
            .push_back(CapturedDriverOutputLine { sequence, line });
    }

    /// Account for `count` lines that were not captured because capturing fell behind.
    fn skip(&self, count: u64) {
        let mut state = self
            .inner
            .lock()
            .expect("browser driver output capture mutex should not be poisoned");
        state.total_lines += usize::try_from(count).unwrap_or(usize::MAX);
    }
}

/// Snapshot of browser driver output captured so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriverOutputSnapshot {
    /// Total number of output lines seen by the capture handle.
    pub(crate) total_lines: usize,

    /// Maximum number of recent output lines retained.
    pub(crate) tail_capacity: usize,

    /// Recent output lines in capture order.
    pub(crate) tail_lines: Vec<CapturedDriverOutputLine>,
}

impl DriverOutputSnapshot {
    /// Whether the retained output tail is empty.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.tail_lines.is_empty()
    }
}

#[derive(Debug, Clone)]
struct DriverOutputAttachment {
    snapshot: DriverOutputSnapshot,
}

struct DriverOutputAttachmentHandler;

impl AttachmentHandler<DriverOutputAttachment> for DriverOutputAttachmentHandler {
    fn display(value: &DriverOutputAttachment, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Recent browser driver output")?;
        writeln!(
            formatter,
            "note: output comes from one shared browser-driver process; parallel tests may interleave lines."
        )?;
        if value.snapshot.total_lines > value.snapshot.tail_lines.len() {
            writeln!(
                formatter,
                "showing the last {} of {} captured line(s).",
                value.snapshot.tail_lines.len(),
                value.snapshot.total_lines,
            )?;
        }

        for captured in &value.snapshot.tail_lines {
            writeln!(
                formatter,
                "[{} {}] {}",
                captured.sequence,
                source_label(captured.line.source),
                captured.line.line,
            )?;
        }

        Ok(())
    }

    fn debug(value: &DriverOutputAttachment, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Self::display(value, formatter)
    }

    fn preferred_formatting_style(
        _value: &DriverOutputAttachment,
        _report_formatting: FormattingFunction,
    ) -> AttachmentFormattingStyle {
        AttachmentFormattingStyle {
            placement: AttachmentFormattingPlacement::Appendix {
                appendix_name: "Recent browser driver output",
            },
            function: FormattingFunction::Display,
            priority: 5,
        }
    }
}

pub(crate) fn attach_browser_driver_output(
    report: &mut Report<BrowserTestError>,
    capture: Option<&DriverOutputCapture>,
) {
    let Some(snapshot) = capture.map(DriverOutputCapture::snapshot) else {
        return;
    };
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
    use super::*;
    use crate::test_support::EnvVarGuard;
    use assertr::prelude::*;

    #[test]
    fn tail_lines_of_zero_disables_capture() {
        assert_that!(DriverOutput::default()).is_equal_to(DriverOutput::disabled());
        assert_that!(DriverOutput::tail_lines(0)).is_equal_to(DriverOutput::disabled());
        assert_that!(
            DriverOutput::tail_lines(3)
                .tail_line_count()
                .map(NonZeroUsize::get)
        )
        .is_equal_to(Some(3));
    }

    #[test]
    fn from_env_reads_flag_and_tail_lines() {
        let env = EnvVarGuard::new(DEFAULT_BROWSER_DRIVER_OUTPUT_ENV);
        let tail_lines = EnvVarGuard::new_unlocked("BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES");

        env.set("1");
        tail_lines.set("12");
        assert_that!(DriverOutput::from_env()).is_equal_to(Ok(Some(DriverOutput::tail_lines(12))));

        tail_lines.remove();
        assert_that!(DriverOutput::from_env()).is_equal_to(Ok(Some(DriverOutput::tail_lines(
            DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES,
        ))));

        tail_lines.set("lots");
        assert_that!(DriverOutput::from_env().is_err()).is_true();

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

        assert_that!(snapshot.total_lines).is_equal_to(5);
        assert_that!(snapshot.tail_capacity).is_equal_to(2);
        assert_that!(snapshot.tail_lines.len()).is_equal_to(2);
        assert_that!(&snapshot.tail_lines[0].line.line).is_equal_to("two");
        assert_that!(snapshot.tail_lines[0].sequence).is_equal_to(1);
        assert_that!(&snapshot.tail_lines[1].line.line).is_equal_to("three");
        assert_that!(snapshot.tail_lines[1].sequence).is_equal_to(4);
        assert_that!(snapshot.tail_lines[0].line.source).is_equal_to(DriverOutputSource::Stderr);
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

        assert_that!(report.attachments().len()).is_equal_to(initial_attachment_count + 1);
        assert_that!(report.children().len()).is_equal_to(0);
        assert_that!(report.to_string()).contains("Recent browser driver output");
        assert_that!(report.to_string()).contains("parallel tests may interleave lines");
    }

    fn line(source: DriverOutputSource, line: &str) -> DriverOutputLine {
        DriverOutputLine::new(source, line)
    }
}

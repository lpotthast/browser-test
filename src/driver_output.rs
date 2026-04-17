use std::collections::VecDeque;
use std::env;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use chrome_for_testing_manager::{DriverOutputLine, DriverOutputListener, DriverOutputSource};
use rootcause::Report;
use rootcause::handlers::{
    AttachmentFormattingPlacement, AttachmentFormattingStyle, AttachmentHandler, FormattingFunction,
};
use rootcause::report_attachment::ReportAttachment;

use crate::BrowserTestError;
use crate::env::env_flag_enabled;

/// Default environment variable enabling browser driver output capture.
pub(crate) const DEFAULT_BROWSER_DRIVER_OUTPUT_ENV: &str = "BROWSER_TEST_DRIVER_OUTPUT";

/// Default environment variable controlling the captured browser driver output tail size.
pub(crate) const DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES_ENV: &str =
    "BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES";

/// Default number of browser driver output lines retained when env capture is enabled.
pub(crate) const DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES: usize = 200;

/// Browser-driver output capture mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverOutputConfig {
    /// Do not capture browser-driver output.
    Disabled,

    /// Capture the last `usize` browser-driver output lines for failure diagnostics.
    ///
    /// `0` disables capture.
    TailLines(usize),

    /// Read capture settings from `BROWSER_TEST_DRIVER_OUTPUT` and
    /// `BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES`.
    FromEnv,
}

/// Deprecated name for [`DriverOutputConfig`].
///
/// Use [`DriverOutputConfig`] in new code.
#[deprecated(since = "0.1.0", note = "use DriverOutputConfig instead")]
pub type BrowserDriverOutputConfig = DriverOutputConfig;

impl DriverOutputConfig {
    /// Disable browser-driver output capture.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    /// Capture the last `tail_lines` browser-driver output lines for failure diagnostics.
    ///
    /// `0` disables capture.
    #[must_use]
    pub const fn tail_lines(tail_lines: usize) -> Self {
        Self::TailLines(tail_lines)
    }

    /// Capture the last `tail_lines` browser-driver output lines for failure diagnostics.
    ///
    /// `0` disables capture.
    #[must_use]
    pub const fn new(tail_lines: usize) -> Self {
        Self::tail_lines(tail_lines)
    }

    /// Read capture settings from the browser-driver output environment variables.
    #[must_use]
    pub const fn from_env() -> Self {
        Self::FromEnv
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
    tail_lines: VecDeque<DriverOutputLine>,
}

impl DriverOutputCapture {
    /// Create a capture handle retaining the last `tail_lines` driver output lines.
    ///
    /// Use [`DriverOutputConfig`] when `0` should mean disabled.
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

    pub(crate) fn listener(&self) -> DriverOutputListener {
        let capture = self.clone();
        DriverOutputListener::new(move |line| {
            capture.push(line);
        })
    }

    pub(crate) fn push(&self, line: DriverOutputLine) {
        let mut state = self
            .inner
            .lock()
            .expect("browser driver output capture mutex should not be poisoned");
        state.total_lines += 1;

        while state.tail_lines.len() >= state.tail_capacity.get() {
            state.tail_lines.pop_front();
        }
        state.tail_lines.push_back(line);
    }
}

/// Snapshot of browser driver output captured so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriverOutputSnapshot {
    /// Total number of output lines seen by the capture handle.
    pub(crate) total_lines: usize,

    /// Maximum number of recent output lines retained.
    pub(crate) tail_capacity: usize,

    /// Recent output lines in callback sequence order.
    pub(crate) tail_lines: Vec<DriverOutputLine>,
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

        for line in &value.snapshot.tail_lines {
            writeln!(
                formatter,
                "[{} {}] {}",
                line.sequence,
                source_label(line.source),
                line.line,
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

pub(crate) fn browser_driver_output_config_from_env() -> DriverOutputConfig {
    if !env_flag_enabled(DEFAULT_BROWSER_DRIVER_OUTPUT_ENV) {
        return DriverOutputConfig::Disabled;
    }

    let tail_lines = env::var_os(DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES_ENV).map_or(
        DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES,
        |value| {
            let value = value.to_string_lossy();
            match value.trim().parse::<usize>() {
                Ok(tail_lines) => tail_lines,
                Err(err) => {
                    tracing::warn!(
                        env_var = DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES_ENV,
                        value = %value,
                        error = %err,
                        fallback = DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES,
                        "invalid browser driver output tail-line setting"
                    );
                    DEFAULT_BROWSER_DRIVER_OUTPUT_TAIL_LINES
                }
            }
        },
    );

    if tail_lines == 0 {
        DriverOutputConfig::Disabled
    } else {
        DriverOutputConfig::TailLines(tail_lines)
    }
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
    use assertr::prelude::*;

    #[allow(deprecated)]
    #[test]
    fn deprecated_browser_driver_output_config_alias_matches_driver_output_config() {
        let config: BrowserDriverOutputConfig = DriverOutputConfig::tail_lines(3);

        assert_that!(config).is_equal_to(DriverOutputConfig::TailLines(3));
    }

    #[test]
    fn capture_retains_bounded_tail_and_total_count() {
        let capture = DriverOutputCapture::new(
            NonZeroUsize::new(2).expect("literal tail capacity should be non-zero"),
        );

        capture.push(line(DriverOutputSource::Stdout, 0, "one"));
        capture.push(line(DriverOutputSource::Stderr, 1, "two"));
        capture.push(line(DriverOutputSource::Stdout, 2, "three"));

        let snapshot = capture.snapshot();

        assert_that!(snapshot.total_lines).is_equal_to(3);
        assert_that!(snapshot.tail_capacity).is_equal_to(2);
        assert_that!(snapshot.tail_lines.len()).is_equal_to(2);
        assert_that!(&snapshot.tail_lines[0].line).is_equal_to("two");
        assert_that!(&snapshot.tail_lines[1].line).is_equal_to("three");
        assert_that!(snapshot.tail_lines[0].source).is_equal_to(DriverOutputSource::Stderr);
    }

    #[test]
    fn browser_driver_output_is_rootcause_attachment_not_child_error() {
        let capture = DriverOutputCapture::new(
            NonZeroUsize::new(5).expect("literal tail capacity should be non-zero"),
        );
        capture.push(line(DriverOutputSource::Stdout, 0, "Starting ChromeDriver"));
        let mut report = Report::new(BrowserTestError::RunTest {
            test_name: "login".to_owned(),
        });
        let initial_attachment_count = report.attachments().len();

        attach_browser_driver_output(&mut report, Some(&capture));

        assert_that!(report.attachments().len()).is_equal_to(initial_attachment_count + 1);
        assert_that!(report.children().len()).is_equal_to(0);
        assert_that!(report.to_string()).contains("Recent browser driver output");
        assert_that!(report.to_string()).contains("parallel tests may interleave lines");
    }

    fn line(source: DriverOutputSource, sequence: u64, line: &str) -> DriverOutputLine {
        DriverOutputLine {
            source,
            sequence,
            line: line.to_owned(),
        }
    }
}

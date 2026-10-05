//! Consumers of the report of a browser test run.

use crate::BrowserTestRunReport;

/// Receives the [`BrowserTestRunReport`] at the end of a run.
///
/// Register consumers with [`crate::BrowserTestRunner::with_report_consumer`]. The runner calls
/// every registered consumer, in registration order, after each run that started tests. Consumers
/// are the only way to get the report: without one, the runner neither prints nor keeps it.
///
/// Ready-made consumers print the run summary (the report's `Display` output): [`StderrSummary`],
/// [`StdoutSummary`], and [`TracingSummary`]. Closures taking a `&BrowserTestRunReport` are
/// consumers as well.
///
/// # Examples
///
/// ```rust
/// use browser_test::{BrowserTestRunReport, BrowserTestRunner, StderrSummary};
///
/// let runner = BrowserTestRunner::new()
///     .with_report_consumer(StderrSummary)
///     .with_report_consumer(|report: &BrowserTestRunReport| {
///         assert!(report.body_time() < std::time::Duration::from_secs(600));
///     });
/// ```
pub trait RunReportConsumer: Send + Sync {
    /// Consume the report of a finished run.
    fn consume(&self, report: &BrowserTestRunReport);
}

impl<F> RunReportConsumer for F
where
    F: Fn(&BrowserTestRunReport) + Send + Sync,
{
    fn consume(&self, report: &BrowserTestRunReport) {
        self(report);
    }
}

/// Prints the run summary to stderr.
///
/// Like all output of a test, the test harness captures it unless the test fails or runs with
/// `--nocapture`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct StderrSummary;

impl RunReportConsumer for StderrSummary {
    fn consume(&self, report: &BrowserTestRunReport) {
        eprintln!("{report}");
    }
}

/// Prints the run summary to stdout.
///
/// Like all output of a test, the test harness captures it unless the test fails or runs with
/// `--nocapture`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct StdoutSummary;

impl RunReportConsumer for StdoutSummary {
    fn consume(&self, report: &BrowserTestRunReport) {
        println!("{report}");
    }
}

/// Logs the run summary through `tracing` at `info` level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TracingSummary;

impl RunReportConsumer for TracingSummary {
    fn consume(&self, report: &BrowserTestRunReport) {
        tracing::info!("{report}");
    }
}

//! Consumers of the report of a browser test run.

use crate::RunReport;

/// Receives the [`RunReport`] at the end of a run.
///
/// Register consumers with [`crate::BrowserTestRunner::with_report_consumer`]. The runner calls
/// every registered consumer, in registration order, after each run that started tests. Without
/// one, the runner neither prints nor logs the report.
/// [`BrowserTestRunner::run_with_report`](crate::BrowserTestRunner::run_with_report) returns it
/// as well.
///
/// Ready-made consumers print the run summary (the report's `Display` output): [`StderrSummary`],
/// [`StdoutSummary`], and [`TracingSummary`]. Closures taking a `&RunReport` are
/// consumers as well.
///
/// # Examples
///
/// ```rust
/// use assertr::prelude::*;
/// use browser_test::{RunReport, BrowserTestRunner, Cancellation, StderrSummary};
///
/// let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals())
///     .with_report_consumer(StderrSummary)
///     .with_report_consumer(|report: &RunReport| {
///         assert_that!(report.body_time()).is_less_than(std::time::Duration::from_secs(600));
///     });
/// ```
pub trait RunReportConsumer: Send + Sync {
    /// Consume the report of a finished run.
    fn consume(&self, report: &RunReport);
}

impl<F> RunReportConsumer for F
where
    F: Fn(&RunReport) + Send + Sync,
{
    fn consume(&self, report: &RunReport) {
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
    fn consume(&self, report: &RunReport) {
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
    fn consume(&self, report: &RunReport) {
        println!("{report}");
    }
}

/// Logs the run summary through `tracing` at `info` level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TracingSummary;

impl RunReportConsumer for TracingSummary {
    fn consume(&self, report: &RunReport) {
        tracing::info!("{report}");
    }
}

#![doc = include_str!("../README.md")]

extern crate self as browser_test;

pub use async_trait::async_trait;
pub use browser_test_macros::browser_test;
pub use chrome_for_testing_manager::{CancellationToken, Channel, ChromeBinary};
pub use thirtyfour;

mod cancellation;
mod driver_output;
mod env;
mod error;
mod execution;
pub mod failure_report;
mod filter;
mod frozen_pages;
mod pause;
mod profile;
mod progress;
mod report;
mod report_consumer;
mod runner;
mod scheduler;
mod session_reuse;
mod step;
mod test_case;
mod test_settings;
#[cfg(test)]
mod test_support;
mod timeout;
mod wait;

pub use cancellation::Cancellation;
pub use driver_output::DriverOutput;
pub use env::InvalidEnvVar;
pub use error::BrowserTestError;
pub use filter::{InvalidPattern, TestFilter};
pub use pause::Pause;
pub use progress::ProgressWarnings;
pub use report::{
    BrowserTestRecord, BrowserTestRunReport, SessionPreparation, SessionTiming, StepStats,
    SuiteRecord, TestOutcome,
};
pub use report_consumer::{RunReportConsumer, StderrSummary, StdoutSummary, TracingSummary};
pub use runner::{BrowserTestRunner, Visibility};
pub use scheduler::{FailurePolicy, Parallelism};
pub use session_reuse::{CachedData, KeptCaches, SessionReset, SessionReuse};
pub use step::{Step, StepExt};
pub use test_case::{BrowserTest, TestSuite};
pub use test_settings::TestSettings;
pub use timeout::Timeouts;
pub use wait::ElementQueryWait;

/// Implementation details used by generated browser tests.
#[doc(hidden)]
pub mod __private {
    use std::future::Future;

    use rootcause::Report;

    /// What `BrowserTest::run` returns.
    pub type DynamicResult = Result<(), Report>;

    /// A test's result, including through result aliases, with its errors as dynamic reports, as
    /// `BrowserTest::run` returns them.
    #[diagnostic::on_unimplemented(
        message = "browser tests must return `Result<(), rootcause::Report>` or `Result<(), rootcause::Report<E>>`, not `{Self}`",
        label = "this browser test returns `{Self}`",
        note = "any error converts into a `rootcause::Report` with `?`"
    )]
    pub trait TestResult {
        /// This result, with its error as a dynamic report.
        ///
        /// # Errors
        ///
        /// Returns the test's error.
        fn into_dynamic(self) -> DynamicResult;
    }

    impl<E: ?Sized + 'static> TestResult for Result<(), Report<E>> {
        fn into_dynamic(self) -> DynamicResult {
            self.map_err(Report::into_dynamic)
        }
    }

    /// Run a test's `body`, and turn its result `into_dynamic`. Its frames are browser-test's,
    /// which failure reports leave out.
    pub async fn run_body<F>(body: F, into_dynamic: fn(F::Output) -> DynamicResult) -> DynamicResult
    where
        F: Future,
    {
        into_dynamic(body.await)
    }
}

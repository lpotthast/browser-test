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
mod pause;
mod profile;
mod progress;
mod report;
mod report_consumer;
mod runner;
mod scheduler;
mod session_reuse;
mod session_settings;
mod step;
mod test_case;
#[cfg(test)]
mod test_support;
mod timeout;
mod wait;

pub use cancellation::Cancellation;
pub use driver_output::DriverOutput;
pub use env::InvalidEnvVar;
pub use error::BrowserTestError;
pub use filter::TestFilter;
pub use pause::Pause;
pub use progress::ProgressWarnings;
pub use report::{
    BrowserTestRecord, BrowserTestRunReport, GroupRecord, SessionPreparation, SessionTiming,
    StepStats, TestOutcome,
};
pub use report_consumer::{RunReportConsumer, StderrSummary, StdoutSummary, TracingSummary};
pub use runner::{BrowserTestRunner, Visibility};
pub use scheduler::{FailurePolicy, Parallelism};
pub use session_reuse::{CachedData, KeptCaches, SessionReset, SessionReuse};
pub use session_settings::SessionSettings;
pub use step::{Step, StepExt};
pub use test_case::{BrowserTest, BrowserTests, NamedTest, TestGroup};
pub use timeout::Timeouts;
pub use wait::ElementQueryWait;

/// Implementation details used by generated browser tests.
#[doc(hidden)]
pub mod __private {
    /// Extract the error marker from a test's result, including through result aliases.
    #[diagnostic::on_unimplemented(
        message = "browser tests must return `Result<(), rootcause::Report<E>>`, not `{Self}`",
        label = "the return type of this browser test",
        note = "`rootcause::Report` without a type parameter is `Report<Dynamic>`, which any error converts into with `?`"
    )]
    pub trait TestResult {
        /// The error marker expected by `BrowserTest`.
        type Error: ?Sized + 'static;
    }

    impl<E: ?Sized + 'static> TestResult for Result<(), rootcause::Report<E>> {
        type Error = E;
    }
}

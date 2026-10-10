#![doc = include_str!("../README.md")]

pub use async_trait::async_trait;
pub use chrome_for_testing_manager::{CancellationToken, Channel, ChromeBinary};
pub use thirtyfour;

mod cancellation;
mod driver_output;
mod env;
mod error;
mod execution;
pub mod failure_report;
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
#[cfg(test)]
mod test_support;
mod timeout;
mod wait;

pub use cancellation::Cancellation;
pub use driver_output::DriverOutput;
pub use env::InvalidEnvVar;
pub use error::BrowserTestError;
pub use pause::Pause;
pub use profile::ChromeProfilesDir;
pub use progress::{ProgressWarnings, ProgressWarningsBuilder};
pub use report::{
    BrowserTestRecord, BrowserTestRunReport, GroupRecord, SessionPreparation, SessionTiming,
    StepStats, TestOutcome,
};
pub use report_consumer::{RunReportConsumer, StderrSummary, StdoutSummary, TracingSummary};
pub use runner::{BrowserTestRunner, Visibility};
pub use scheduler::{FailurePolicy, Parallelism};
pub use session_reuse::{CachedData, KeptCaches, SessionReset, SessionReuse};
pub use step::{Step, StepExt};
pub use test_case::{BrowserTest, BrowserTests};
pub use timeout::{Timeouts, TimeoutsBuilder};
pub use wait::{ElementQueryWait, ElementQueryWaitError};

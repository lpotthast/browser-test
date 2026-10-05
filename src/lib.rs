#![doc = include_str!("../README.md")]

pub use async_trait::async_trait;
pub use chrome_for_testing_manager::{Channel, ChromeBinary};
pub use thirtyfour;

mod driver_output;
mod env;
mod error;
mod execution;
mod pause;
mod progress;
mod report;
mod report_consumer;
mod runner;
mod scheduler;
mod step;
mod test_case;
#[cfg(test)]
mod test_support;
mod timeout;
mod wait;

pub use driver_output::DriverOutput;
pub use env::InvalidEnvVar;
pub use error::BrowserTestError;
pub use pause::Pause;
pub use progress::{ProgressWarnings, ProgressWarningsBuilder};
pub use report::{
    BrowserTestRecord, BrowserTestRunReport, GroupRecord, SessionTiming, StepStats, TestOutcome,
};
pub use report_consumer::{RunReportConsumer, StderrSummary, StdoutSummary, TracingSummary};
pub use runner::{BrowserTestRunner, Visibility};
pub use scheduler::{FailurePolicy, Parallelism};
pub use step::{Step, StepExt};
pub use test_case::{BrowserTest, BrowserTests};
pub use timeout::{Timeouts, TimeoutsBuilder};
pub use wait::{ElementQueryWait, ElementQueryWaitError};

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
mod runner;
mod scheduler;
mod session;
mod step;
mod test_case;
#[cfg(test)]
mod test_support;
mod timeout;
mod wait;

#[allow(deprecated)]
pub use driver_output::BrowserDriverOutputConfig;
pub use driver_output::{DriverOutputConfig, ResolvedDriverOutputConfig};
pub use error::BrowserTestError;
pub use pause::{PauseConfig, ResolvedPauseConfig};
pub use progress::{ProgressWarnings, ProgressWarningsBuilder};
pub use report::{
    BrowserTestRecord, BrowserTestRunOutcome, BrowserTestRunReport, SessionAcquisition, StepStats,
    TestOutcome,
};
pub use runner::{
    BrowserTestRunner, BrowserTestVisibility, ResolvedBrowserTestVisibility, RunSummary,
};
pub use scheduler::{BrowserTestFailurePolicy, BrowserTestParallelism};
pub use session::{SessionRequirement, SessionReset, SessionReuse};
pub use step::step;
pub use test_case::{BrowserTest, BrowserTests};
pub use timeout::{BrowserTimeouts, BrowserTimeoutsBuilder};
pub use wait::{
    ElementQueryWaitConfig, ElementQueryWaitConfigBuilder, ElementQueryWaitConfigError,
};

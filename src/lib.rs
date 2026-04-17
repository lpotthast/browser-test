#![doc = include_str!("../README.md")]

pub use async_trait::async_trait;
pub use chrome_for_testing_manager::Channel;
pub use thirtyfour;

mod driver_output;
mod env;
mod error;
mod execution;
mod pause;
mod runner;
mod scheduler;
mod test_case;
#[cfg(test)]
mod test_support;
mod timeout;
mod wait;

#[allow(deprecated)]
pub use driver_output::BrowserDriverOutputConfig;
pub use driver_output::DriverOutputConfig;
pub use error::BrowserTestError;
pub use pause::PauseConfig;
pub use runner::{BrowserTestRunner, BrowserTestVisibility};
pub use scheduler::{BrowserTestFailurePolicy, BrowserTestParallelism};
pub use test_case::{BrowserTest, BrowserTests};
pub use timeout::{BrowserTimeouts, BrowserTimeoutsBuilder};
pub use wait::{
    ElementQueryWaitConfig, ElementQueryWaitConfigBuilder, ElementQueryWaitConfigError,
};

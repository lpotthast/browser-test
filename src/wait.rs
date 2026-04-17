use std::{sync::Arc, time::Duration};

use thirtyfour::{error::WebDriverResult, extensions::query::ElementPollerWithTimeout};
use typed_builder::TypedBuilder;

/// Wait configuration used by thirtyfour element queries and element waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, TypedBuilder)]
pub struct ElementQueryWaitConfig {
    /// Maximum time an element query or element wait keeps polling before failing.
    ///
    /// This controls `thirtyfour`'s explicit element-query polling, such as queries created via
    /// `driver.query(...)` and waits that use the configured driver poller. It is not a
    /// `WebDriver` protocol timeout, and it does not control page navigation, script execution, or
    /// ordinary Rust futures in the test body.
    ///
    /// In browser tests this is the timeout that usually determines how long the test waits for a
    /// dynamic DOM condition, such as an element appearing after hydration, a button becoming
    /// clickable, or content being inserted after an application request completes.
    #[builder(setter(into))]
    timeout: Duration,

    /// Delay between element-query poll attempts during the timeout window.
    ///
    /// Smaller intervals can make tests react faster once the expected element state appears, but
    /// they also issue `WebDriver` commands more frequently. Larger intervals reduce browser-driver
    /// traffic, but can make successful waits complete later than necessary.
    ///
    /// Avoid `Duration::ZERO` for values from configuration, environment variables, or other
    /// dynamic input. A zero interval is accepted by [`Self::new`] and the builder for trusted
    /// construction, but can create an immediate retry loop in the underlying element poller. Use
    /// [`Self::try_new`] when the interval is not a hard-coded trusted value.
    #[builder(setter(into))]
    interval: Duration,
}

/// Invalid element query wait configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ElementQueryWaitConfigError {
    /// The poll interval must be non-zero.
    #[error("Element query wait poll interval must be non-zero.")]
    ZeroInterval,
}

impl ElementQueryWaitConfig {
    /// Create a new element query wait configuration without validation.
    ///
    /// Use [`Self::try_new`] for values from user input or environment configuration. A zero
    /// interval is accepted here for const construction, but can cause immediate retry loops in the
    /// underlying `WebDriver` element poller.
    #[must_use]
    pub const fn new(timeout: Duration, interval: Duration) -> Self {
        Self { timeout, interval }
    }

    /// Create a new element query wait configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ElementQueryWaitConfigError::ZeroInterval`] if `interval` is
    /// [`Duration::ZERO`].
    pub fn try_new(
        timeout: Duration,
        interval: Duration,
    ) -> Result<Self, ElementQueryWaitConfigError> {
        if interval.is_zero() {
            return Err(ElementQueryWaitConfigError::ZeroInterval);
        }

        Ok(Self { timeout, interval })
    }

    /// Maximum time an element query or element wait keeps polling before failing.
    #[must_use]
    pub const fn timeout(self) -> Duration {
        self.timeout
    }

    /// Delay between element-query poll attempts during the timeout window.
    #[must_use]
    pub const fn interval(self) -> Duration {
        self.interval
    }

    pub(crate) fn into_thirtyfour_webdriver_config(
        self,
    ) -> WebDriverResult<thirtyfour::common::config::WebDriverConfig> {
        thirtyfour::common::config::WebDriverConfig::builder()
            .poller(Arc::new(ElementPollerWithTimeout::new(
                self.timeout,
                self.interval,
            )))
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assertr::prelude::*;

    #[test]
    fn builder_preserves_timeout_and_interval() {
        let wait = ElementQueryWaitConfig::builder()
            .timeout(Duration::from_secs(10))
            .interval(Duration::from_millis(250))
            .build();

        assert_that!(wait.timeout()).is_equal_to(Duration::from_secs(10));
        assert_that!(wait.interval()).is_equal_to(Duration::from_millis(250));
    }

    #[test]
    fn builder_preserves_zero_interval_for_trusted_callers() {
        let wait = ElementQueryWaitConfig::builder()
            .timeout(Duration::from_secs(10))
            .interval(Duration::ZERO)
            .build();

        assert_that!(wait.interval()).is_equal_to(Duration::ZERO);
    }

    #[test]
    fn try_new_accepts_non_zero_interval() {
        let wait =
            ElementQueryWaitConfig::try_new(Duration::from_secs(10), Duration::from_millis(250))
                .expect("non-zero interval should be accepted");

        assert_that!(wait.timeout()).is_equal_to(Duration::from_secs(10));
        assert_that!(wait.interval()).is_equal_to(Duration::from_millis(250));
    }

    #[test]
    fn try_new_rejects_zero_interval() {
        let err = ElementQueryWaitConfig::try_new(Duration::from_secs(10), Duration::ZERO)
            .expect_err("zero interval should be rejected");

        assert_that!(err).is_equal_to(ElementQueryWaitConfigError::ZeroInterval);
    }

    #[test]
    fn new_preserves_zero_interval_for_const_trusted_callers() {
        const WAIT: ElementQueryWaitConfig =
            ElementQueryWaitConfig::new(Duration::from_secs(10), Duration::ZERO);

        assert_that!(WAIT.interval()).is_equal_to(Duration::ZERO);
    }
}

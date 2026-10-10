use std::time::Duration;

use thirtyfour::extensions::query::{ElementPollerWithTimeout, IntoElementPoller};

/// How `thirtyfour` element queries and element waits poll.
///
/// This controls `thirtyfour`'s explicit element-query polling, such as queries created via
/// `driver.query(...)` and waits that use the configured driver poller. It is not a `WebDriver`
/// protocol timeout, and it does not control page navigation, script execution, or ordinary Rust
/// futures in the test body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementQueryWait {
    timeout: Duration,
    interval: Duration,
}

/// Invalid element query wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ElementQueryWaitError {
    /// The poll interval must be non-zero.
    #[error("Element query wait poll interval must be non-zero.")]
    ZeroInterval,
}

impl ElementQueryWait {
    /// Poll every `interval` until `timeout` elapses.
    ///
    /// `timeout` usually determines how long a test waits for a dynamic DOM condition, such as an
    /// element appearing after hydration, a button becoming clickable, or content being inserted
    /// after an application request completes. Smaller intervals make tests react faster once the
    /// expected state appears, but issue `WebDriver` commands more frequently.
    ///
    /// # Errors
    ///
    /// Returns [`ElementQueryWaitError::ZeroInterval`] if `interval` is [`Duration::ZERO`], which
    /// would make the element poller retry without pause.
    pub fn new(timeout: Duration, interval: Duration) -> Result<Self, ElementQueryWaitError> {
        if interval.is_zero() {
            return Err(ElementQueryWaitError::ZeroInterval);
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

    pub(crate) fn into_thirtyfour_poller(self) -> impl IntoElementPoller + Send + Sync {
        ElementPollerWithTimeout::new(self.timeout, self.interval)
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    #[test]
    fn new_accepts_non_zero_interval() {
        let wait = ElementQueryWait::new(Duration::from_secs(10), Duration::from_millis(250))
            .expect("non-zero interval should be accepted");

        assert_that!(wait.timeout()).is_equal_to(Duration::from_secs(10));
        assert_that!(wait.interval()).is_equal_to(Duration::from_millis(250));
    }

    #[test]
    fn new_rejects_zero_interval() {
        let err = ElementQueryWait::new(Duration::from_secs(10), Duration::ZERO)
            .expect_err("zero interval should be rejected");

        assert_that!(err).is_equal_to(ElementQueryWaitError::ZeroInterval);
    }
}

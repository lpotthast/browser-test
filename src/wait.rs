use std::time::Duration;

use thirtyfour::extensions::query::{ElementPollerWithTimeout, IntoElementPoller};

/// How `thirtyfour` element queries and element waits poll.
///
/// This controls `thirtyfour`'s explicit element-query polling, such as queries created with
/// `driver.query(...)` and waits that use the session's poller. It is not a `WebDriver` timeout
/// (see [`Timeouts`](crate::Timeouts)), and does not control page navigation, script execution,
/// or the Rust futures of the test body.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use browser_test::ElementQueryWait;
///
/// let wait = ElementQueryWait::new(Duration::from_secs(10), Duration::from_millis(500));
/// assert_eq!(wait.timeout(), Duration::from_secs(10));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementQueryWait {
    timeout: Duration,
    interval: Duration,
}

impl ElementQueryWait {
    /// Poll every `interval` until `timeout` elapses.
    ///
    /// `timeout` decides how long a test waits for a dynamic DOM condition, such as an element
    /// appearing after hydration, a button becoming clickable, or content inserted after a request
    /// of the application completes. Smaller intervals make tests react faster once the expected
    /// state appears, but send `WebDriver` commands more often.
    ///
    /// # Panics
    ///
    /// Panics if `interval` is zero, which would make queries poll without pause.
    #[must_use]
    #[track_caller]
    pub const fn new(timeout: Duration, interval: Duration) -> Self {
        assert!(
            !interval.is_zero(),
            "the poll interval of an element query wait must be non-zero"
        );
        Self { timeout, interval }
    }

    /// How long an element query or element wait keeps polling before it fails.
    #[must_use]
    pub const fn timeout(self) -> Duration {
        self.timeout
    }

    /// The delay between two polls.
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
    fn new_keeps_timeout_and_interval() {
        let wait = ElementQueryWait::new(Duration::from_secs(10), Duration::from_millis(250));

        assert_that!(wait.timeout()).is_equal_to(Duration::from_secs(10));
        assert_that!(wait.interval()).is_equal_to(Duration::from_millis(250));
    }

    #[test]
    #[should_panic(expected = "must be non-zero")]
    fn new_rejects_a_zero_interval() {
        let _ = ElementQueryWait::new(Duration::from_secs(10), Duration::ZERO);
    }
}

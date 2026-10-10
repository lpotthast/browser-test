//! What a test needs of its session.

use crate::{ElementQueryWait, Timeouts};

/// What a test needs of the session it runs in, returned by
/// [`BrowserTest::session_settings`](crate::BrowserTest::session_settings).
///
/// The default, [`Self::new`], takes everything from the runner. Settings a test sets override the
/// runner's for that test only.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use browser_test::{ElementQueryWait, SessionSettings, Timeouts};
///
/// let settings = SessionSettings::new()
///     .with_timeouts(Timeouts::new().with_page_load(Duration::from_secs(30)))
///     .with_element_query_wait(ElementQueryWait::new(
///         Duration::from_secs(20),
///         Duration::from_millis(100),
///     ))
///     .with_fresh_session(true);
/// assert!(settings.fresh_session());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SessionSettings {
    timeouts: Timeouts,
    element_query_wait: Option<ElementQueryWait>,
    fresh_session: bool,
}

impl SessionSettings {
    /// The runner's settings, unchanged.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            timeouts: Timeouts::new(),
            element_query_wait: None,
            fresh_session: false,
        }
    }

    /// Set `WebDriver` timeouts for this test.
    ///
    /// The timeouts set in `timeouts` replace the runner's (see
    /// [`BrowserTestRunner::with_timeouts`](crate::BrowserTestRunner::with_timeouts)) one by one.
    /// Those left unset keep the runner's values.
    #[must_use]
    pub const fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// Set how element queries poll in this test, instead of the runner's
    /// [`BrowserTestRunner::with_element_query_wait`](crate::BrowserTestRunner::with_element_query_wait).
    ///
    /// The element query wait is fixed when a session is created. A test whose wait differs from
    /// the runner's does not take one of the sessions the runner keeps ready (see
    /// [`BrowserTestRunner::with_spare_sessions`](crate::BrowserTestRunner::with_spare_sessions)),
    /// but gets a session created for it when its turn comes.
    #[must_use]
    pub const fn with_element_query_wait(mut self, wait: ElementQueryWait) -> Self {
        self.element_query_wait = Some(wait);
        self
    }

    /// Whether the test needs a session no other test ran in. Defaults to `false`.
    ///
    /// Only matters with [`SessionReuse::enabled`](crate::SessionReuse::enabled), where sessions
    /// are reset after their test and run further tests. Require a fresh session for a test that
    /// needs a browser no test ran in, e.g. one measuring a first page load, with empty caches: the
    /// pool gives it a session no test ran in, creating one if none is ready. Afterwards its
    /// session returns to the pool like any other.
    #[must_use]
    pub const fn with_fresh_session(mut self, fresh_session: bool) -> Self {
        self.fresh_session = fresh_session;
        self
    }

    /// The test's timeouts, overriding the runner's. See [`Self::with_timeouts`].
    #[must_use]
    pub const fn timeouts(self) -> Timeouts {
        self.timeouts
    }

    /// The test's element query wait, if it overrides the runner's. See
    /// [`Self::with_element_query_wait`].
    #[must_use]
    pub const fn element_query_wait(self) -> Option<ElementQueryWait> {
        self.element_query_wait
    }

    /// Whether the test needs a session no other test ran in. See [`Self::with_fresh_session`].
    #[must_use]
    pub const fn fresh_session(self) -> bool {
        self.fresh_session
    }
}

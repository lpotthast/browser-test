//! What a test needs of the runner.

use std::time::Duration;

use crate::{ElementQueryWait, Timeouts};

/// What a test needs of the runner: settings of the session it runs in, and how long it may run.
/// Returned by [`BrowserTest::settings`](crate::BrowserTest::settings).
///
/// The default, [`Self::new`], takes everything from the runner. Settings a test sets override the
/// runner's for that test only. A test wrapping another one forwards all of them at once by
/// returning the wrapped test's settings.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use assertr::prelude::*;
/// use browser_test::{ElementQueryWait, TestSettings, Timeouts};
///
/// let settings = TestSettings::new()
///     .with_timeouts(Timeouts::new().with_page_load(Duration::from_secs(30)))
///     .with_element_query_wait(ElementQueryWait::new(
///         Duration::from_secs(20),
///         Duration::from_millis(100),
///     ))
///     .with_fresh_session(true)
///     .with_time_limit(Duration::from_secs(10));
/// assert_that!(settings.fresh_session()).is_true();
/// assert_that!(settings.time_limit()).is_equal_to(Some(Duration::from_secs(10)));
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TestSettings {
    timeouts: Timeouts,
    element_query_wait: Option<ElementQueryWait>,
    fresh_session: bool,
    time_limit: Option<Duration>,
}

impl TestSettings {
    /// The runner's settings, unchanged.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            timeouts: Timeouts::new(),
            element_query_wait: None,
            fresh_session: false,
            time_limit: None,
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

    /// Set how long the test's body may run before it fails as timed out, instead of the runner's
    /// limit ([`BrowserTestRunner::with_time_limit`](crate::BrowserTestRunner::with_time_limit), a
    /// last resort). Set a limit fitting the test where you know how long it takes, so that a test
    /// that hangs (e.g. on a page it froze) fails soon. `#[browser_test(time_limit = ...)]` sets it.
    #[must_use]
    pub const fn with_time_limit(mut self, limit: Duration) -> Self {
        self.time_limit = Some(limit);
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

    /// The test's time limit, if it overrides the runner's. See [`Self::with_time_limit`].
    #[must_use]
    pub const fn time_limit(self) -> Option<Duration> {
        self.time_limit
    }
}

/// The runner's settings for tests that don't set their own (see [`TestSettings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TestDefaults {
    /// See [`BrowserTestRunner::with_timeouts`](crate::BrowserTestRunner::with_timeouts).
    pub(crate) timeouts: Timeouts,
    /// See [`BrowserTestRunner::with_element_query_wait`](crate::BrowserTestRunner::with_element_query_wait).
    pub(crate) element_query_wait: Option<ElementQueryWait>,
    /// See [`BrowserTestRunner::with_time_limit`](crate::BrowserTestRunner::with_time_limit).
    pub(crate) time_limit: Duration,
}

impl TestDefaults {
    /// Ten minutes: a last resort against tests that hang, longer than any test should run.
    pub(crate) const DEFAULT_TIME_LIMIT: Duration = Duration::from_secs(600);

    pub(crate) const fn new() -> Self {
        Self {
            timeouts: Timeouts::new(),
            element_query_wait: None,
            time_limit: Self::DEFAULT_TIME_LIMIT,
        }
    }
}

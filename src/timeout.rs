use std::time::Duration;

/// `WebDriver` timeouts applied to a session before a test runs.
///
/// Every timeout is optional. A timeout left unset is not updated, so the session keeps the
/// runner's value (see [`BrowserTestRunner::with_timeouts`](crate::BrowserTestRunner::with_timeouts)),
/// or else `ChromeDriver`'s default. A test's [`SessionSettings::with_timeouts`](crate::SessionSettings::with_timeouts)
/// overrides the runner's timeouts one by one: those it sets replace the runner's, the others stay.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use browser_test::Timeouts;
///
/// let timeouts = Timeouts::new()
///     .with_page_load(Duration::from_secs(10))
///     .with_implicit_wait(Duration::ZERO);
///
/// assert_eq!(timeouts.page_load(), Some(Duration::from_secs(10)));
/// assert_eq!(timeouts.script(), None);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Timeouts {
    script: Option<Duration>,
    page_load: Option<Duration>,
    implicit_wait: Option<Duration>,
}

impl Timeouts {
    /// Timeouts updating nothing. Set them with the `with_*` methods.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            script: None,
            page_load: None,
            implicit_wait: None,
        }
    }

    /// Set how long `WebDriver` waits for a script to finish.
    ///
    /// This applies to browser-side scripts that explicitly wait for completion, such as
    /// `execute_async` calls, which fail when the script does not finish in time. It does not
    /// control page navigation, element lookups, or the Rust futures of the test body.
    #[must_use]
    pub const fn with_script(mut self, timeout: Duration) -> Self {
        self.script = Some(timeout);
        self
    }

    /// Set how long `WebDriver` waits for a navigation to finish loading the page.
    ///
    /// This applies to navigation commands such as opening a URL, refreshing, or going back. It
    /// covers the browser's page-load lifecycle, not the readiness of the application after the
    /// document has loaded: wait for hydration, background requests, and delayed DOM updates with
    /// element-query waits or polling of your own.
    #[must_use]
    pub const fn with_page_load(mut self, timeout: Duration) -> Self {
        self.page_load = Some(timeout);
        self
    }

    /// Set how long `WebDriver` element lookups wait for a matching element to appear.
    ///
    /// A non-zero implicit wait makes every lookup of a missing element block until it expires,
    /// slowing down checks that an element is absent, and compounds with explicit polling. When
    /// waiting with `thirtyfour`'s element queries and [`ElementQueryWait`](crate::ElementQueryWait),
    /// prefer `Duration::ZERO`, keeping waits explicit and local to the step that needs them.
    #[must_use]
    pub const fn with_implicit_wait(mut self, timeout: Duration) -> Self {
        self.implicit_wait = Some(timeout);
        self
    }

    /// The script timeout, if set. See [`Self::with_script`].
    #[must_use]
    pub const fn script(self) -> Option<Duration> {
        self.script
    }

    /// The page-load timeout, if set. See [`Self::with_page_load`].
    #[must_use]
    pub const fn page_load(self) -> Option<Duration> {
        self.page_load
    }

    /// The implicit wait timeout, if set. See [`Self::with_implicit_wait`].
    #[must_use]
    pub const fn implicit_wait(self) -> Option<Duration> {
        self.implicit_wait
    }

    /// These timeouts, with the unset ones taken from `fallback`.
    #[must_use]
    pub(crate) fn or(self, fallback: Self) -> Self {
        Self {
            script: self.script.or(fallback.script),
            page_load: self.page_load.or(fallback.page_load),
            implicit_wait: self.implicit_wait.or(fallback.implicit_wait),
        }
    }

    /// Whether no timeout is set.
    pub(crate) const fn is_empty(self) -> bool {
        self.script.is_none() && self.page_load.is_none() && self.implicit_wait.is_none()
    }

    pub(crate) fn into_thirtyfour_timeout_configuration(self) -> thirtyfour::TimeoutConfiguration {
        thirtyfour::TimeoutConfiguration::new(self.script, self.page_load, self.implicit_wait)
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    #[test]
    fn setters_set_their_timeout_only() {
        let timeouts = Timeouts::new()
            .with_script(Duration::from_secs(5))
            .with_implicit_wait(Duration::ZERO);

        assert_that!(timeouts.script()).is_equal_to(Some(Duration::from_secs(5)));
        assert_that!(timeouts.page_load()).is_none();
        assert_that!(timeouts.implicit_wait()).is_equal_to(Some(Duration::ZERO));
        assert_that!(Timeouts::new()).is_equal_to(Timeouts::default());
        assert_that!(Timeouts::new().is_empty()).is_true();
    }

    #[test]
    fn unset_timeouts_fall_back_one_by_one() {
        let runner = Timeouts::new()
            .with_script(Duration::from_secs(10))
            .with_implicit_wait(Duration::ZERO);
        let test = Timeouts::new()
            .with_script(Duration::from_secs(1))
            .with_page_load(Duration::from_secs(5));

        assert_that!(test.or(runner)).is_equal_to(
            Timeouts::new()
                .with_script(Duration::from_secs(1))
                .with_page_load(Duration::from_secs(5))
                .with_implicit_wait(Duration::ZERO),
        );
    }

    #[test]
    fn conversion_preserves_unset_fields() {
        let timeouts = Timeouts::new()
            .with_script(Duration::from_secs(5))
            .into_thirtyfour_timeout_configuration();

        assert_that!(timeouts.script()).is_equal_to(Some(Duration::from_secs(5)));
        assert_that!(timeouts.page_load()).is_none();
        assert_that!(timeouts.implicit()).is_none();
    }
}

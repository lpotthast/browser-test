use std::time::Duration;

use typed_builder::TypedBuilder;

/// `WebDriver` timeout configuration applied before running a browser test.
///
/// The builder setters accept `Duration` values. Use each setter's `_opt` fallback for
/// `Option<Duration>` values. Leaving a timeout unset means the runner does not update that
/// timeout on the `WebDriver` session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, TypedBuilder)]
#[allow(clippy::struct_field_names)]
pub struct BrowserTimeouts {
    /// Maximum time `WebDriver` waits for asynchronous script execution.
    ///
    /// This timeout applies to browser-side scripts that explicitly wait for completion, such as
    /// `execute_async` / async JavaScript calls. It does not control page navigation, element
    /// lookup, or ordinary Rust futures in the test body.
    ///
    /// In browser tests this usually matters when helpers inject JavaScript that calls back later,
    /// waits for browser APIs, or bridges to application state from inside the page. If the script
    /// does not finish before this duration, the script command fails even if the page itself is
    /// otherwise healthy.
    ///
    /// Use the builders `script_timeout(...)` to update the timeout. Use `script_timeout_opt(None)`
    /// to leave the session's current script timeout unchanged.
    #[builder(default, setter(strip_option(fallback_suffix = "_opt")))]
    script_timeout: Option<Duration>,

    /// Maximum time `WebDriver` waits for page navigation to finish loading.
    ///
    /// This timeout applies to navigation commands such as opening a URL, refreshing, or moving
    /// through browser history. It covers the browser's page-load lifecycle, not arbitrary
    /// application readiness after the document has loaded.
    ///
    /// In browser tests this can fail a `driver.goto(...)` call when the target page, redirects,
    /// or blocking resources take too long. It is not a replacement for explicit waits after
    /// navigation: single-page app hydration, background requests, animations, and delayed DOM
    /// updates should still be handled with element-query waits or test-specific polling.
    ///
    /// Use the builders `page_load_timeout(...)` to update the timeout. Use
    /// `page_load_timeout_opt(None)` to leave the session's current page-load timeout unchanged.
    #[builder(default, setter(strip_option(fallback_suffix = "_opt")))]
    page_load_timeout: Option<Duration>,

    /// Maximum time `WebDriver` waits while locating elements through raw element lookup commands.
    ///
    /// This timeout affects implicit waiting in the browser driver itself. When it is non-zero,
    /// element lookup commands can block until a matching element appears or the duration expires.
    /// That can make missing-element assertions slower and can compound with explicit polling.
    ///
    /// For tests using `thirtyfour` element queries, `WebElement::wait_until`, or this crate's
    /// [`ElementQueryWaitConfig`](crate::ElementQueryWaitConfig), prefer keeping this at
    /// `Duration::ZERO` and using explicit waits instead. Explicit waits make the waiting behavior
    /// local to the assertion or action that needs it, while a non-zero implicit wait affects every
    /// element lookup in the session.
    ///
    /// Use the builders `implicit_wait_timeout(...)` to update the timeout. Passing
    /// `Duration::ZERO` explicitly disables implicit waiting for the session. Use
    /// `implicit_wait_timeout_opt(None)` to leave the session's current implicit wait timeout
    /// unchanged.
    #[builder(default, setter(strip_option(fallback_suffix = "_opt")))]
    implicit_wait_timeout: Option<Duration>,
}

impl BrowserTimeouts {
    /// Maximum time `WebDriver` waits for asynchronous script execution.
    ///
    /// Returning `None` means this timeout is not updated.
    #[must_use]
    pub const fn script_timeout(self) -> Option<Duration> {
        self.script_timeout
    }

    /// Maximum time `WebDriver` waits for page navigation to finish loading.
    ///
    /// Returning `None` means this timeout is not updated.
    #[must_use]
    pub const fn page_load_timeout(self) -> Option<Duration> {
        self.page_load_timeout
    }

    /// Maximum time `WebDriver` waits while locating elements through raw element lookup commands.
    ///
    /// Returning `None` means this timeout is not updated.
    #[must_use]
    pub const fn implicit_wait_timeout(self) -> Option<Duration> {
        self.implicit_wait_timeout
    }

    pub(crate) fn into_thirtyfour_timeout_configuration(self) -> thirtyfour::TimeoutConfiguration {
        thirtyfour::TimeoutConfiguration::new(
            self.script_timeout,
            self.page_load_timeout,
            self.implicit_wait_timeout,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assertr::prelude::*;

    #[test]
    fn builder_preserves_all_timeout_fields() {
        let timeouts = BrowserTimeouts::builder()
            .script_timeout(Duration::from_secs(5))
            .page_load_timeout(Duration::from_secs(10))
            .implicit_wait_timeout(Duration::from_secs(20))
            .build();

        assert_that!(timeouts.script_timeout()).is_equal_to(Some(Duration::from_secs(5)));
        assert_that!(timeouts.page_load_timeout()).is_equal_to(Some(Duration::from_secs(10)));
        assert_that!(timeouts.implicit_wait_timeout()).is_equal_to(Some(Duration::from_secs(20)));
    }

    #[test]
    fn builder_leaves_unset_fields_unconfigured() {
        let timeouts = BrowserTimeouts::builder().build();

        assert_that!(timeouts.script_timeout()).is_none();
        assert_that!(timeouts.page_load_timeout()).is_none();
        assert_that!(timeouts.implicit_wait_timeout()).is_none();
    }

    #[test]
    fn builder_accepts_wrapped_option_values() {
        let timeouts = BrowserTimeouts::builder()
            .script_timeout_opt(Some(Duration::from_secs(5)))
            .page_load_timeout_opt(None)
            .implicit_wait_timeout_opt(Some(Duration::ZERO))
            .build();

        assert_that!(timeouts.script_timeout()).is_equal_to(Some(Duration::from_secs(5)));
        assert_that!(timeouts.page_load_timeout()).is_none();
        assert_that!(timeouts.implicit_wait_timeout()).is_equal_to(Some(Duration::ZERO));
    }

    #[test]
    fn conversion_preserves_unset_fields() {
        let timeouts = BrowserTimeouts::builder()
            .script_timeout(Duration::from_secs(5))
            .build()
            .into_thirtyfour_timeout_configuration();

        assert_that!(timeouts.script()).is_equal_to(Some(Duration::from_secs(5)));
        assert_that!(timeouts.page_load()).is_none();
        assert_that!(timeouts.implicit()).is_none();
    }
}

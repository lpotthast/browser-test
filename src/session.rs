//! Session reuse: which tests may share a `WebDriver` session, and how a shared session is reset
//! between tests.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use rootcause::Report;
use rootcause::prelude::ResultExt;
use thirtyfour::{Rect, TimeoutConfiguration, WebDriver, WindowHandle};

use crate::env::env_flag_value;
use crate::{BrowserTimeouts, ElementQueryWaitConfig};

pub(crate) const DEFAULT_SESSION_REUSE_ENV: &str = "BROWSER_TEST_SESSION_REUSE";

/// Whether a [`crate::BrowserTest`] may run in a `WebDriver` session that other tests use too.
///
/// Returned by [`crate::BrowserTest::session`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SessionRequirement {
    /// The test may run in a session that earlier tests used, and its session may be handed on to
    /// later tests once it passed.
    ///
    /// The runner resets a session before handing it on; see the crate documentation ("Session
    /// Reuse") for exactly which browser state the reset clears. Only tests with the same effective
    /// [`crate::BrowserTest::timeouts`] and [`crate::BrowserTest::element_query_wait`] share a
    /// session.
    #[default]
    Shared,

    /// The test runs in a new session that no other test used before and that is quit after the
    /// test.
    ///
    /// Use this for tests that depend on a pristine browser (e.g. state the session reset does not
    /// clear, such as permissions or the HTTP cache) or that change the browser in ways the reset
    /// does not undo (e.g. Chrome flags through CDP).
    Fresh,
}

/// Whether [`crate::BrowserTestRunner`] reuses `WebDriver` sessions between tests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum SessionReuse {
    /// Tests whose [`crate::BrowserTest::session`] is [`SessionRequirement::Shared`] share
    /// sessions.
    #[default]
    Enabled,

    /// Every test gets its own fresh session.
    Disabled,

    /// Read the setting from the given environment variable.
    ///
    /// Reuse stays enabled unless the variable is set to `0`, `false`, `no`, `off`, or
    /// `disabled`.
    FromEnvVar(String),

    /// Read the setting from `BROWSER_TEST_SESSION_REUSE`.
    ///
    /// Reuse stays enabled unless the variable is set to `0`, `false`, `no`, `off`, or
    /// `disabled`.
    FromEnv,
}

impl SessionReuse {
    /// Build a session reuse config from `BROWSER_TEST_SESSION_REUSE`.
    #[must_use]
    pub const fn from_env() -> Self {
        Self::FromEnv
    }

    /// Build a session reuse config from an environment variable.
    #[must_use]
    pub fn from_env_var(env_var: impl Into<String>) -> Self {
        Self::FromEnvVar(env_var.into())
    }

    /// Resolve this config to whether sessions are reused.
    ///
    /// Environment-backed configs read their environment variable when this method is called. An
    /// unset or empty variable keeps the default (enabled).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        match self {
            Self::Enabled => true,
            Self::Disabled => false,
            Self::FromEnvVar(env_var) => env_flag_value(env_var).unwrap_or(true),
            Self::FromEnv => env_flag_value(DEFAULT_SESSION_REUSE_ENV).unwrap_or(true),
        }
    }
}

impl From<bool> for SessionReuse {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// App-specific reset of a shared `WebDriver` session, run before the session is handed to the
/// next test.
///
/// Register implementations with [`crate::BrowserTestRunner::with_session_reset`]. They run after
/// the runner's built-in reset, in registration order, while the driver shows a new blank tab.
/// Returning an error discards the session: the next test then gets a fresh one.
///
/// # Examples
///
/// ```rust
/// use browser_test::{SessionReset, async_trait, thirtyfour::WebDriver};
/// use rootcause::Report;
///
/// /// Logs out of the app under test, whose session lives on the server.
/// struct LogOut {
///     logout_url: String,
/// }
///
/// #[async_trait]
/// impl SessionReset for LogOut {
///     async fn reset(&self, driver: &WebDriver) -> Result<(), Report> {
///         driver.goto(&self.logout_url).await?;
///         Ok(())
///     }
/// }
/// ```
// `async_trait` marks the boxed futures `#[must_use]`, which they are already.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait SessionReset: Send + Sync {
    /// Reset app-specific state of the session.
    ///
    /// # Errors
    ///
    /// Return an error if the state cannot be reset. The runner then discards the session.
    async fn reset(&self, driver: &WebDriver) -> Result<(), Report>;
}

/// The settings a session is created with. Only tests with equal settings share a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SessionSettings {
    pub(crate) timeouts: Option<BrowserTimeouts>,
    pub(crate) element_query_wait: Option<ElementQueryWaitConfig>,
}

/// Browser state captured when a reusable session was created, restored by every reset.
pub(crate) struct SessionBaseline {
    window_rect: Rect,
    timeouts: TimeoutConfiguration,
    /// Origins of the documents open at the end of any test in this session. Their storage is
    /// cleared on every reset.
    origins: BTreeSet<String>,
}

impl SessionBaseline {
    pub(crate) async fn capture(driver: &WebDriver) -> Result<Self, Report> {
        Ok(Self {
            window_rect: driver
                .get_window_rect()
                .await
                .context("failed to read the window rect")?,
            timeouts: driver
                .get_timeouts()
                .await
                .context("failed to read the session timeouts")?,
            origins: BTreeSet::new(),
        })
    }

    /// Reset the session so that the next test starts from a blank state.
    ///
    /// Releases pressed keys and buttons, opens a new blank tab and closes every other window
    /// (dropping their documents, history, `sessionStorage`, and per-tab `DevTools` overrides),
    /// clears cookies of every origin and the
    /// storage of every origin seen in this session, restores the window rect and timeouts, and
    /// runs the app-specific resets.
    pub(crate) async fn reset(
        &mut self,
        driver: &WebDriver,
        resets: &[Arc<dyn SessionReset>],
    ) -> Result<(), Report> {
        // Release keys and buttons the previous test left pressed (WebDriver input state belongs
        // to the session, not to a tab).
        driver
            .action_chain()
            .reset_actions()
            .await
            .context("failed to release pressed keys and buttons")?;
        let blank_tab = driver.new_tab().await.context("failed to open a new tab")?;
        self.close_windows_except(driver, &blank_tab).await?;

        let cdp = driver.cdp();
        cdp.network()
            .clear_browser_cookies()
            .await
            .context("failed to clear the cookies")?;
        for origin in &self.origins {
            cdp.storage()
                .clear_all_data_for_origin(origin.as_str())
                .await
                .context_with(|| format!("failed to clear the storage of {origin}"))?;
        }

        let window_rect = driver
            .get_window_rect()
            .await
            .context("failed to read the window rect")?;
        if window_rect != self.window_rect {
            let Rect {
                x,
                y,
                width,
                height,
            } = self.window_rect;
            driver
                .set_window_rect(
                    x,
                    y,
                    u32::try_from(width).unwrap_or(u32::MAX),
                    u32::try_from(height).unwrap_or(u32::MAX),
                )
                .await
                .context("failed to restore the window rect")?;
        }
        driver
            .update_timeouts(self.timeouts.clone())
            .await
            .context("failed to restore the session timeouts")?;

        for reset in resets {
            reset.reset(driver).await?;
        }
        Ok(())
    }

    async fn close_windows_except(
        &mut self,
        driver: &WebDriver,
        keep: &WindowHandle,
    ) -> Result<(), Report> {
        let windows = driver
            .windows()
            .await
            .context("failed to list the windows")?;
        for window in windows.into_iter().filter(|window| window != keep) {
            driver
                .switch_to_window(window)
                .await
                .context("failed to switch to a window to close it")?;
            let url = driver
                .current_url()
                .await
                .context("failed to read the url of a window")?;
            let origin = url.origin();
            if origin.is_tuple() {
                self.origins.insert(origin.ascii_serialization());
            }
            driver
                .close_window()
                .await
                .context("failed to close a window")?;
        }
        driver
            .switch_to_window(keep.clone())
            .await
            .context("failed to switch to the new tab")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvVarGuard;
    use assertr::prelude::*;

    #[test]
    fn session_requirement_defaults_to_shared() {
        assert_that!(SessionRequirement::default()).is_equal_to(SessionRequirement::Shared);
    }

    #[test]
    fn session_reuse_defaults_to_enabled() {
        assert_that!(SessionReuse::default().is_enabled()).is_true();
        assert_that!(SessionReuse::Disabled.is_enabled()).is_false();
        assert_that!(SessionReuse::from(false)).is_equal_to(SessionReuse::Disabled);
        assert_that!(SessionReuse::from(true)).is_equal_to(SessionReuse::Enabled);
    }

    #[test]
    fn session_reuse_from_env_keeps_default_when_unset() {
        let env = EnvVarGuard::new(DEFAULT_SESSION_REUSE_ENV);
        env.remove();
        assert_that!(SessionReuse::from_env().is_enabled()).is_true();

        env.set("");
        assert_that!(SessionReuse::from_env().is_enabled()).is_true();
    }

    #[test]
    fn session_reuse_from_env_reads_flag() {
        let env = EnvVarGuard::new(DEFAULT_SESSION_REUSE_ENV);
        env.set("0");
        assert_that!(SessionReuse::from_env().is_enabled()).is_false();

        env.set("off");
        assert_that!(SessionReuse::from_env().is_enabled()).is_false();

        env.set("1");
        assert_that!(SessionReuse::from_env().is_enabled()).is_true();
    }

    #[test]
    fn session_reuse_from_custom_env_var() {
        let env = EnvVarGuard::new("BROWSER_TEST_CUSTOM_SESSION_REUSE");
        env.set("no");
        assert_that!(SessionReuse::from_env_var("BROWSER_TEST_CUSTOM_SESSION_REUSE").is_enabled())
            .is_false();
    }
}

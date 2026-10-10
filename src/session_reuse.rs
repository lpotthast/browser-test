//! Reusing a test's session for the next test, after resetting it.

use std::{collections::BTreeSet, num::NonZeroUsize};

use rootcause::{Report, prelude::ResultExt as _};
use thirtyfour::{
    BrowserCapabilitiesHelper as _, CapabilitiesHelper as _, ChromeCapabilities,
    ChromiumLikeCapabilities as _, Rect, TimeoutConfiguration, WebDriver, WindowHandle,
    bidi::{
        UserContextId,
        modules::browsing_context::{Create, CreateType},
    },
    error::WebDriverResult,
};

use crate::{InvalidEnvVar, env::env_flag};

pub(crate) const DEFAULT_SESSION_REUSE_ENV: &str = "BROWSER_TEST_SESSION_REUSE";

/// Whether a test's session runs further tests after it, see
/// [`crate::BrowserTestRunner::with_session_reuse`].
///
/// Creating a session starts a new browser. With reuse enabled, sessions return to the runner's
/// pool after their test: reset, they run further tests as long as upcoming tests need them, and
/// quit once the pool has enough. A run then creates about as many sessions as tests run at the
/// same time, plus spares, however many tests it has. How a session is reset is its
/// [`SessionReset`].
///
/// A test that needs a browser no test ran in (e.g. one measuring a first page load, with empty
/// caches) says so with [`SessionSettings::with_fresh_session`](crate::SessionSettings::with_fresh_session):
/// the pool gives it a session no test ran in, creating one if none is ready. Afterwards its session returns to the pool like
/// any other. A session whose reset fails (e.g. its browser crashed) quits.
///
/// With reuse enabled, sessions start Chrome without its back/forward cache
/// (`--disable-features=BackForwardCache`, merged into a `--disable-features` of your own), unless
/// [`Self::with_back_forward_cache`] enables it: both resets then behave alike within a test (a
/// page left is unloaded, going back loads it again), and no cached page keeps a renderer process
/// alive across tests of a [`SessionReset::Manual`] session. A test with an element query wait of
/// its own gets a session created for it, which is set up the same way (also running the test in
/// a user context of its own with [`SessionReset::NewContext`]), and quits after the test.
///
/// The settings ([`Self::with_reset`], [`Self::with_max_tests_per_session`],
/// [`Self::with_back_forward_cache`]) only take effect while reuse is enabled. They are kept while
/// it is disabled, so that settings can be applied to whatever [`Self::from_env`] chose:
///
/// ```
/// use browser_test::{CachedData, SessionReset, SessionReuse};
///
/// let reuse = SessionReuse::from_env()?
///     .unwrap_or(SessionReuse::enabled())
///     .with_reset(SessionReset::manual([CachedData::Http]));
/// # Ok::<(), browser_test::InvalidEnvVar>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SessionReuse {
    enabled: bool,
    max_tests_per_session: Option<NonZeroUsize>,
    reset: SessionReset,
    back_forward_cache: bool,
}

impl SessionReuse {
    /// Every test runs in a fresh session. The default.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            max_tests_per_session: None,
            reset: SessionReset::NewContext,
            back_forward_cache: false,
        }
    }

    /// Sessions are reset after their test and run further tests, by default with
    /// [`SessionReset::NewContext`].
    #[must_use]
    pub const fn enabled() -> Self {
        Self {
            enabled: true,
            max_tests_per_session: None,
            reset: SessionReset::NewContext,
            back_forward_cache: false,
        }
    }

    /// Reset sessions with `reset` instead of [`SessionReset::NewContext`].
    #[must_use]
    pub const fn with_reset(mut self, reset: SessionReset) -> Self {
        self.reset = reset;
        self
    }

    /// Keep Chrome's back/forward cache enabled in reusable sessions (disabled by default), e.g.
    /// for tests of pages restored from it. With [`SessionReset::Manual`], every page a test
    /// navigates away from then keeps its renderer process (100 to 250 MB) for the rest of the
    /// session.
    #[must_use]
    pub const fn with_back_forward_cache(mut self, enabled: bool) -> Self {
        self.back_forward_cache = enabled;
        self
    }

    /// Quit a session after it ran `max` tests, e.g. to bound how much memory a long-running
    /// browser accumulates. Unlimited by default.
    #[must_use]
    pub const fn with_max_tests_per_session(mut self, max: NonZeroUsize) -> Self {
        self.max_tests_per_session = Some(max);
        self
    }

    /// Read whether sessions are reused from `BROWSER_TEST_SESSION_REUSE`.
    ///
    /// See [`Self::from_env_var`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a boolean flag.
    pub fn from_env() -> Result<Option<Self>, InvalidEnvVar> {
        Self::from_env_var(DEFAULT_SESSION_REUSE_ENV)
    }

    /// Read whether sessions are reused from the boolean flag `env_var`: enabled means
    /// [`Self::enabled`].
    ///
    /// Returns `None` if the variable is unset or empty, so the caller picks the default:
    /// `SessionReuse::from_env()?.unwrap_or(SessionReuse::enabled())`. `1`, `true`, `yes`, `on`,
    /// and `enabled` enable the flag, `0`, `false`, `no`, `off`, and `disabled` disable it
    /// (ignoring case). The variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a boolean flag.
    pub fn from_env_var(env_var: impl AsRef<str>) -> Result<Option<Self>, InvalidEnvVar> {
        Ok(env_flag(env_var.as_ref())?.map(|enabled| {
            if enabled {
                Self::enabled()
            } else {
                Self::disabled()
            }
        }))
    }

    /// Whether sessions are reused at all.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// The most tests a session runs, if limited.
    #[must_use]
    pub const fn max_tests_per_session(self) -> Option<NonZeroUsize> {
        self.max_tests_per_session
    }

    /// How sessions are reset between tests.
    #[must_use]
    pub const fn reset(self) -> SessionReset {
        self.reset
    }

    /// Whether reusable sessions keep Chrome's back/forward cache.
    #[must_use]
    pub const fn back_forward_cache(self) -> bool {
        self.back_forward_cache
    }

    /// Whether a session that ran `tests_run` tests may run another one.
    pub(crate) fn allows_another_test(self, tests_run: usize) -> bool {
        self.enabled
            && self
                .max_tests_per_session
                .is_none_or(|max| tests_run < max.get())
    }
}

/// How a reused session is reset between two tests, see [`SessionReuse`].
///
/// Both strategies give the next test a browser without the state of the tests before (cookies,
/// storage, permissions, pages, history, emulation, pressed keys, window size, timeouts). They
/// differ in how, and in what may be kept: run a suite with each to cross-check that its tests
/// don't depend on the strategy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SessionReset {
    /// Every test runs in a tab of a user context of its own (`WebDriver` `BiDi`'s isolated
    /// browser profiles, like incognito windows); the reset removes it. The default.
    ///
    /// Complete by construction: removing the context drops its tabs and windows (with their
    /// history, page state and CDP overrides), cookies, storage of every origin, caches and
    /// permissions, and the renderer processes of its pages, whatever a test changed. Nothing is
    /// kept: every test starts with empty caches, e.g. downloads and compiles the app's scripts
    /// and WebAssembly again.
    ///
    /// The session's first tab stays open in the browser's default context, so a test sees two
    /// windows. A test that leaves state in the default context, which removing a user context
    /// doesn't remove, makes its session quit instead of being reset: by opening windows with
    /// `WebDriver`'s New Window command (they open in the default context), or by navigating the
    /// first tab. Sessions are created with `WebDriver` `BiDi` enabled (`webSocketUrl`).
    #[default]
    NewContext,

    /// Every test of a session runs in the session's one tab; the reset clears what tests can
    /// change, item by item, and keeps the [`CachedData`] listed, which only tests that load the
    /// same unchanging resources can rely on being safe.
    ///
    /// The reset:
    /// - closes every other window (popups, `WebDriver`'s New Window);
    /// - navigates the tab to an empty page in a new renderer process (a `data:` URL; the test's
    ///   page and its process go, with the caches in it), and clears its history;
    /// - clears cookies, and the storage (`localStorage`, `sessionStorage`, `IndexedDB`, cache
    ///   storage, service workers, file systems) of every origin the tab's history and the closed
    ///   windows showed;
    /// - resets permissions granted through CDP, and the CDP emulation overrides (device metrics,
    ///   user agent, geolocation, media, timezone, locale, touch, CPU throttling, idle state,
    ///   focus emulation, background color, script execution);
    /// - clears the HTTP cache, unless kept ([`CachedData::Http`]).
    ///
    /// State it doesn't list survives into the next test: e.g. other CDP domains' settings, storage
    /// of an origin only an iframe showed, Shared Storage, Storage Buckets, Interest Groups (the
    /// shader cache is kept: compiled GPU shaders, invisible to pages). Use [`Self::NewContext`] for
    /// tests that change such state, or to cross-check.
    Manual(KeptCaches),
}

impl SessionReset {
    /// [`Self::Manual`], keeping the `cached` data between tests.
    #[must_use]
    pub fn manual(cached: impl IntoIterator<Item = CachedData>) -> Self {
        Self::Manual(cached.into_iter().collect())
    }
}

/// Data a browser caches that [`SessionReset::Manual`] may keep between tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CachedData {
    /// The HTTP cache: responses the server lets browsers cache (`Cache-Control`), and the code
    /// V8 compiled from cached scripts and WebAssembly. Safe to keep while the served files don't
    /// change during the run (e.g. content-hashed file names), as a production server's.
    Http,
}

/// The [`CachedData`] a [`SessionReset::Manual`] keeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KeptCaches {
    http: bool,
}

impl KeptCaches {
    /// Whether `data` is kept.
    #[must_use]
    pub const fn contains(self, data: CachedData) -> bool {
        match data {
            CachedData::Http => self.http,
        }
    }
}

impl FromIterator<CachedData> for KeptCaches {
    fn from_iter<I: IntoIterator<Item = CachedData>>(cached: I) -> Self {
        let mut kept = Self::default();
        for data in cached {
            match data {
                CachedData::Http => kept.http = true,
            }
        }
        kept
    }
}

/// Configure the capabilities of a session of a run with `reuse` enabled: reusable or not (a
/// session created for a test with settings of its own), so that every test sees the same browser.
pub(crate) fn configure_reusable_session(
    caps: &mut ChromeCapabilities,
    reuse: SessionReuse,
) -> WebDriverResult<()> {
    if reuse.reset == SessionReset::NewContext {
        // The user contexts are created over BiDi.
        caps.enable_bidi()?;
    }
    if !reuse.back_forward_cache {
        disable_feature(caps, "BackForwardCache")?;
    }
    Ok(())
}

/// Add `feature` to the last `--disable-features` argument, or add one: Chrome only reads the
/// last.
fn disable_feature(caps: &mut ChromeCapabilities, feature: &str) -> WebDriverResult<()> {
    const SWITCH: &str = "--disable-features=";
    let existing = caps
        .args()
        .into_iter()
        .rev()
        .find(|arg| arg.starts_with(SWITCH));
    match existing {
        Some(arg) => {
            caps.remove_arg(&arg)?;
            caps.add_arg(&format!("{arg},{feature}"))
        }
        None => caps.add_arg(&format!("{SWITCH}{feature}")),
    }
}

/// What a session looked like when it was created, restored by [`SessionBaseline::reset`], and
/// where its current test runs.
pub(crate) struct SessionBaseline {
    window_rect: Rect,
    timeouts: TimeoutConfiguration,
    /// The tab the session started with.
    first_tab: WindowHandle,
    /// The page the first tab started with.
    first_url: String,
    isolation: Isolation,
}

/// Why a session was not reset.
pub(crate) enum ResetError {
    /// The test left state in the browser's default context, which removing its user context
    /// doesn't remove ([`SessionReset::NewContext`]). Not a failure: the session quits.
    DefaultContextUsed(&'static str),
    Failed(Report),
}

impl<C: ?Sized + 'static> From<Report<C>> for ResetError {
    fn from(report: Report<C>) -> Self {
        Self::Failed(report.into_dynamic())
    }
}

/// Where a session's tests run.
enum Isolation {
    /// In a tab of this user context ([`SessionReset::NewContext`]).
    UserContext(UserContextId),
    /// In the first tab ([`SessionReset::Manual`]).
    Manual(KeptCaches),
}

impl SessionBaseline {
    /// Record the state of a just created session, and select where its first test runs.
    pub(crate) async fn prepare(driver: &WebDriver, reset: SessionReset) -> Result<Self, Report> {
        let first_tab = driver
            .window()
            .await
            .context("the first tab could not be read")?;
        let first_url = driver
            .current_url()
            .await
            .context("the page of the first tab could not be read")?
            .into();
        let timeouts = driver
            .get_timeouts()
            .await
            .context("the timeouts could not be read")?;
        let isolation = match reset {
            SessionReset::NewContext => Isolation::UserContext(open_user_context(driver).await?),
            SessionReset::Manual(kept) => Isolation::Manual(kept),
        };
        // The window of the first test's tab: every test's tab gets this size and position.
        let window_rect = driver
            .get_window_rect()
            .await
            .context("the window size could not be read")?;
        Ok(Self {
            window_rect,
            timeouts,
            first_tab,
            first_url,
            isolation,
        })
    }

    /// Reset the session for the next test (see [`SessionReset`]).
    pub(crate) async fn reset(&mut self, driver: &WebDriver) -> Result<(), ResetError> {
        // First, so that the reset's own commands (e.g. its navigation) run with the session's
        // timeouts, not those the test set.
        driver
            .update_timeouts(self.timeouts.clone())
            .await
            .context("the timeouts could not be restored")?;
        match &mut self.isolation {
            Isolation::UserContext(user_context) => {
                let bidi = driver
                    .bidi()
                    .await
                    .context("the BiDi connection could not be opened")?;
                bidi.browser()
                    .remove_user_context(user_context.clone())
                    .await
                    .context("the user context of the test could not be removed")?;
                check_default_context_unused(driver, &self.first_tab, &self.first_url).await?;
                *user_context = open_user_context(driver).await?;
            }
            Isolation::Manual(kept) => reset_manually(driver, &self.first_tab, *kept).await?,
        }
        driver
            .action_chain()
            .reset_actions()
            .await
            .context("pressed keys and buttons could not be released")?;
        if driver
            .get_window_rect()
            .await
            .context("the window size could not be read")?
            != self.window_rect
        {
            let Rect {
                x,
                y,
                width,
                height,
            } = self.window_rect;
            driver
                .set_window_rect(x, y, clamp_size(width), clamp_size(height))
                .await
                .context("the window size could not be restored")?;
        }
        Ok(())
    }
}

/// The page a [`SessionReset::Manual`] reset leaves the tab on: empty, and, unlike `about:blank`
/// (which stays in the process of the page before), of an opaque origin, so the tab switches to a
/// new renderer process and the old one exits with the test's page: its memory, and the caches
/// that live in it (decoded resources, compiled code), are not kept.
pub(crate) const BLANK: &str = "data:text/html,";

/// CDP commands resetting the emulation overrides [`SessionReset::Manual`] lists.
fn emulation_resets() -> [(&'static str, serde_json::Value); 13] {
    use serde_json::json;
    [
        ("Emulation.clearDeviceMetricsOverride", json!({})),
        ("Emulation.setUserAgentOverride", json!({ "userAgent": "" })),
        ("Emulation.clearGeolocationOverride", json!({})),
        (
            "Emulation.setEmulatedMedia",
            json!({ "media": "", "features": [] }),
        ),
        ("Emulation.setTimezoneOverride", json!({ "timezoneId": "" })),
        ("Emulation.setLocaleOverride", json!({})),
        (
            "Emulation.setTouchEmulationEnabled",
            json!({ "enabled": false }),
        ),
        (
            "Emulation.setEmitTouchEventsForMouse",
            json!({ "enabled": false }),
        ),
        ("Emulation.setCPUThrottlingRate", json!({ "rate": 1 })),
        ("Emulation.clearIdleOverride", json!({})),
        (
            "Emulation.setFocusEmulationEnabled",
            json!({ "enabled": false }),
        ),
        ("Emulation.setDefaultBackgroundColorOverride", json!({})),
        (
            "Emulation.setScriptExecutionDisabled",
            json!({ "value": false }),
        ),
    ]
}

/// The reset of [`SessionReset::Manual`], in the session's `tab`.
async fn reset_manually(
    driver: &WebDriver,
    tab: &WindowHandle,
    kept: KeptCaches,
) -> Result<(), Report> {
    // An open alert would fail every following command.
    let _ = driver.dismiss_alert().await;
    let mut origins = BTreeSet::new();
    let windows = driver
        .windows()
        .await
        .context("the open windows could not be listed")?;
    for window in windows.iter().filter(|window| *window != tab) {
        driver
            .switch_to_window(window.clone())
            .await
            .context("a window of the test could not be selected")?;
        if let Ok(url) = driver.current_url().await {
            insert_origin(&mut origins, url.as_str());
        }
    }
    close_windows_except(driver, std::slice::from_ref(tab)).await?;
    driver
        .switch_to_window(tab.clone())
        .await
        .context("the session's tab could not be selected")?;

    // Every origin the tab showed, then a blank page without history, in a new renderer process.
    let history = cdp(driver, "Page.getNavigationHistory", serde_json::json!({})).await?;
    for entry in history["entries"].as_array().into_iter().flatten() {
        if let Some(url) = entry["url"].as_str() {
            insert_origin(&mut origins, url);
        }
    }
    driver
        .goto(BLANK)
        .await
        .context("the tab could not be cleared")?;
    cdp(driver, "Page.resetNavigationHistory", serde_json::json!({})).await?;

    for origin in &origins {
        clear_storage(driver, origin).await?;
    }
    cdp(driver, "Network.clearBrowserCookies", serde_json::json!({})).await?;
    if !kept.contains(CachedData::Http) {
        cdp(driver, "Network.clearBrowserCache", serde_json::json!({})).await?;
    }
    cdp(driver, "Browser.resetPermissions", serde_json::json!({})).await?;
    for (method, params) in emulation_resets() {
        cdp(driver, method, params).await?;
    }
    cdp(driver, "Page.bringToFront", serde_json::json!({})).await?;
    Ok(())
}

/// The storage types [`SessionReset::Manual`] clears of every origin, always: `local_storage`
/// clears `sessionStorage` too. Each takes about a millisecond (the `all` of
/// `Storage.clearDataForOrigin` takes seconds on pages of a large app, and clears caches, too).
const STORAGE_TYPES: &str = "local_storage,indexeddb,cache_storage,service_workers";

/// Clear the storage of `origin`: [`STORAGE_TYPES`], and file systems (the Origin Private File
/// System, the sandboxed file system) if the origin uses them. Clearing those takes about 100 ms
/// even when empty.
async fn clear_storage(driver: &WebDriver, origin: &str) -> Result<(), Report> {
    cdp(
        driver,
        "Storage.clearDataForOrigin",
        serde_json::json!({ "origin": origin, "storageTypes": STORAGE_TYPES }),
    )
    .await?;
    let usage = cdp(
        driver,
        "Storage.getUsageAndQuota",
        serde_json::json!({ "origin": origin }),
    )
    .await?;
    let uses_file_systems = usage["usageBreakdown"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| {
            entry["storageType"] == "file_systems" && entry["usage"].as_f64().unwrap_or(0.0) > 0.0
        });
    if uses_file_systems {
        cdp(
            driver,
            "Storage.clearDataForOrigin",
            serde_json::json!({ "origin": origin, "storageTypes": "file_systems" }),
        )
        .await?;
    }
    Ok(())
}

/// Add the origin of `url` to `origins`. `about:blank`, `data:` and other opaque origins have no
/// storage to clear.
fn insert_origin(origins: &mut BTreeSet<String>, url: &str) {
    if let Ok(url) = url::Url::parse(url) {
        let origin = url.origin();
        if origin.is_tuple() {
            origins.insert(origin.ascii_serialization());
        }
    }
}

/// Check that the test of a [`SessionReset::NewContext`] session, whose user context was just
/// removed with its tabs, left the browser's default context as it was: the first tab on its first
/// page, and no other windows. Its cookies, storage and caches would outlive the reset.
async fn check_default_context_unused(
    driver: &WebDriver,
    first_tab: &WindowHandle,
    first_url: &str,
) -> Result<(), ResetError> {
    let windows = driver
        .windows()
        .await
        .context("the open windows could not be listed")?;
    if windows.iter().any(|window| window != first_tab) {
        return Err(ResetError::DefaultContextUsed(
            "the test opened windows in the browser's default context (e.g. with WebDriver's New \
             Window command)",
        ));
    }
    driver
        .switch_to_window(first_tab.clone())
        .await
        .context("the session's first tab could not be selected")?;
    let url = driver
        .current_url()
        .await
        .context("the page of the first tab could not be read")?;
    if url.as_str() != first_url {
        return Err(ResetError::DefaultContextUsed(
            "the test navigated the session's first tab, in the browser's default context",
        ));
    }
    Ok(())
}

/// Close every window but `keep`, and select the first of `keep`.
async fn close_windows_except(driver: &WebDriver, keep: &[WindowHandle]) -> Result<(), Report> {
    let windows = driver
        .windows()
        .await
        .context("the open windows could not be listed")?;
    let others: Vec<_> = windows
        .into_iter()
        .filter(|window| !keep.contains(window))
        .collect();
    if others.is_empty() {
        return Ok(());
    }
    for window in others {
        driver
            .switch_to_window(window)
            .await
            .context("a window of the test could not be selected")?;
        driver
            .close_window()
            .await
            .context("a window of the test could not be closed")?;
    }
    if let Some(first) = keep.first() {
        driver
            .switch_to_window(first.clone())
            .await
            .context("the test's tab could not be selected again")?;
    }
    Ok(())
}

/// Send the CDP command `method` to the current page.
async fn cdp(
    driver: &WebDriver,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, Report> {
    Ok(driver
        .cdp()
        .send_raw(method, params)
        .await
        .context_with(|| format!("the CDP command {method} failed"))?)
}

/// Create a user context with a tab, and select the tab. Returns the user context.
async fn open_user_context(driver: &WebDriver) -> Result<UserContextId, Report> {
    let bidi = driver
        .bidi()
        .await
        .context("the BiDi connection could not be opened")?;
    let user_context = bidi
        .browser()
        .create_user_context()
        .await
        .context("a user context could not be created")?
        .user_context;
    let tab = bidi
        .send(Create {
            r#type: CreateType::Tab,
            reference_context: None,
            background: None,
            user_context: Some(user_context.clone()),
        })
        .await
        .context("a tab could not be opened in the new user context")?;
    driver
        .switch_to_window(WindowHandle::from(tab.context.as_str().to_owned()))
        .await
        .context("the tab of the new user context could not be selected")?;
    Ok(user_context)
}

/// A window dimension as `set_window_rect` takes it.
fn clamp_size(size: i64) -> u32 {
    u32::try_from(size.max(0)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    #[test]
    fn manual_resets_keep_exactly_the_listed_caches() {
        let SessionReset::Manual(kept) = SessionReset::manual([CachedData::Http]) else {
            panic!("manual() is a manual reset");
        };
        assert_that!(kept.contains(CachedData::Http)).is_true();
        let SessionReset::Manual(nothing) = SessionReset::manual([]) else {
            panic!("manual() is a manual reset");
        };
        assert_that!(nothing.contains(CachedData::Http)).is_false();
    }

    #[test]
    fn the_back_forward_cache_joins_a_disable_features_of_your_own() {
        let mut caps = ChromeCapabilities::new();
        disable_feature(&mut caps, "BackForwardCache").expect("caps accept arguments");
        assert_that!(caps.args())
            .is_equal_to(vec!["--disable-features=BackForwardCache".to_owned()]);

        let mut caps = ChromeCapabilities::new();
        caps.add_arg("--disable-features=Translate")
            .expect("caps accept arguments");
        disable_feature(&mut caps, "BackForwardCache").expect("caps accept arguments");
        assert_that!(caps.args()).is_equal_to(vec![
            "--disable-features=Translate,BackForwardCache".to_owned(),
        ]);

        // Chrome reads only the last, so the feature joins that one.
        let mut caps = ChromeCapabilities::new();
        caps.add_arg("--disable-features=Translate")
            .expect("caps accept arguments");
        caps.add_arg("--disable-features=AutofillServerCommunication")
            .expect("caps accept arguments");
        disable_feature(&mut caps, "BackForwardCache").expect("caps accept arguments");
        assert_that!(caps.args()).is_equal_to(vec![
            "--disable-features=Translate".to_owned(),
            "--disable-features=AutofillServerCommunication,BackForwardCache".to_owned(),
        ]);
    }
}

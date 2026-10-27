//! Where the engine gets its browser sessions: the seam between it and Chrome for Testing, so that
//! the engine's tests can drive it with sessions of their own.

use std::{future::Future, panic::AssertUnwindSafe, sync::Arc};

use chrome_for_testing_manager::{
    CancellationToken, ChromeForTesting, ChromeForTestingError, Session,
};
use rootcause::{Report, markers::Dynamic};
use thirtyfour::{
    ChromeCapabilities, ChromiumLikeCapabilities,
    error::{WebDriverError, WebDriverResult},
    extensions::query::{ElementPollerWithTimeout, IntoElementPoller},
};

use super::ChromeCapabilitiesSetup;
use crate::{
    BrowserTest, ElementQueryWait, SessionReset, SessionReuse, Timeouts,
    failure_report::panic_payload_message,
    frozen_pages::{self, PageCheck},
    profile::RunProfiles,
    session_reuse::{ResetError, SessionBaseline, configure_reusable_session},
};

/// Creates browser sessions, and does in them what the engine needs done.
pub(super) trait SessionFactory: Sync {
    type Session: Sync;
    /// What resetting a session restores.
    type Baseline: Send;

    /// Create a session, run `serve` in it, and quit it. `Err` if the session could not be
    /// created, `serve` failed, or quitting failed.
    fn run_session<F>(&self, serve: F) -> impl Future<Output = Result<(), Report>>
    where
        F: for<'s> AsyncFnOnce(&'s Self::Session) -> Result<(), Report>;

    /// Make a new session ready for its first test.
    fn ready(&self, session: &Self::Session) -> impl Future<Output = Result<(), Report>>;

    /// Record the state of a new session that resets restore, and select where its first test
    /// runs.
    fn baseline(
        &self,
        session: &Self::Session,
        reset: SessionReset,
    ) -> impl Future<Output = Result<Self::Baseline, Report>>;

    /// Reset `session` to `baseline` for the next test.
    fn reset(
        &self,
        session: &Self::Session,
        baseline: &mut Self::Baseline,
    ) -> impl Future<Output = Result<(), ResetError>>;

    /// Make the element queries of `session` wait `element_query_wait`, or as `thirtyfour` does by
    /// default if `None`.
    fn set_element_query_wait(
        &self,
        session: &Self::Session,
        element_query_wait: Option<ElementQueryWait>,
    );

    /// Set the timeouts of `session`.
    fn set_timeouts(
        &self,
        session: &Self::Session,
        timeouts: Timeouts,
    ) -> impl Future<Output = Result<(), Report>>;

    /// Run the body of `test` in `session`.
    fn run_test<'a, Context>(
        &'a self,
        session: &'a Self::Session,
        test: &'a dyn BrowserTest<Context>,
        context: &'a Context,
    ) -> impl Future<Output = Result<(), Report>> + 'a
    where
        Context: Sync + ?Sized;

    /// Check the pages of a session whose test ran too long, and close the frozen ones.
    fn close_frozen_pages(&self, session: &Self::Session) -> impl Future<Output = PageCheck>;

    /// The id of `session`, by which the driver's output names it.
    fn session_id(&self, session: &Self::Session) -> String;
}

/// Sessions of Chrome for Testing, each with a profile of its own.
pub(super) struct ChromeSessions<'a> {
    pub(super) chrome: &'a ChromeForTesting,
    pub(super) visible: bool,
    pub(super) chrome_capabilities_setups: &'a [Arc<ChromeCapabilitiesSetup>],
    /// Where sessions get their profiles.
    pub(super) profiles: &'a RunProfiles,
    pub(super) session_reuse: SessionReuse,
    /// Cancels the sessions of a cancelled run.
    pub(super) cancellation: &'a CancellationToken,
}

impl SessionFactory for ChromeSessions<'_> {
    type Session = Session;
    type Baseline = SessionBaseline;

    async fn run_session<F>(&self, serve: F) -> Result<(), Report>
    where
        F: for<'s> AsyncFnOnce(&'s Session) -> Result<(), Report>,
    {
        let profile = self
            .profiles
            .create_session_profile()
            .await
            .map_err(|error| {
                Report::<Dynamic>::from(error)
                    .context("the Chrome profile of the session could not be created")
                    .into_dynamic()
            })?;
        // Boxed, as a session's future takes about 17 KB.
        let result = Box::pin(
            self.chrome
                .session()
                .with_caps(|caps: &mut ChromeCapabilities| {
                    configure_chrome_capabilities(
                        caps,
                        self.visible,
                        self.chrome_capabilities_setups,
                    )?;
                    if self.session_reuse.is_enabled() {
                        configure_reusable_session(caps, self.session_reuse)?;
                    }
                    profile.configure(caps)
                })
                .with_cancellation(self.cancellation.clone())
                .run(serve),
        )
        .await;
        if result.as_ref().is_err_and(quit_timed_out) {
            // Its browser may still write to the profile.
            profile.keep();
        } else {
            profile.remove().await;
        }
        result.map_err(Report::into_dynamic)
    }

    async fn ready(&self, session: &Session) -> Result<(), Report> {
        focus_page(session).await.map_err(|error| {
            Report::<Dynamic>::from(error)
                .context("the page of the session could not be brought to the front")
                .into_dynamic()
        })
    }

    async fn baseline(
        &self,
        session: &Session,
        reset: SessionReset,
    ) -> Result<SessionBaseline, Report> {
        SessionBaseline::prepare(session, reset).await
    }

    async fn reset(
        &self,
        session: &Session,
        baseline: &mut SessionBaseline,
    ) -> Result<(), ResetError> {
        baseline.reset(session).await
    }

    fn set_element_query_wait(
        &self,
        session: &Session,
        element_query_wait: Option<ElementQueryWait>,
    ) {
        let poller: Arc<dyn IntoElementPoller + Send + Sync> = match element_query_wait {
            Some(wait) => Arc::new(wait.into_thirtyfour_poller()),
            None => Arc::new(ElementPollerWithTimeout::default()),
        };
        session.set_poller(poller);
    }

    async fn set_timeouts(&self, session: &Session, timeouts: Timeouts) -> Result<(), Report> {
        session
            .update_timeouts(timeouts.into_thirtyfour_timeout_configuration())
            .await
            .map_err(|error| Report::<Dynamic>::from(error).into_dynamic())
    }

    fn run_test<'a, Context>(
        &'a self,
        session: &'a Session,
        test: &'a dyn BrowserTest<Context>,
        context: &'a Context,
    ) -> impl Future<Output = Result<(), Report>> + 'a
    where
        Context: Sync + ?Sized,
    {
        test.run(session, context)
    }

    async fn close_frozen_pages(&self, session: &Session) -> PageCheck {
        frozen_pages::close_frozen_pages(session).await
    }

    fn session_id(&self, session: &Session) -> String {
        session.session_id().to_string()
    }
}

/// Whether `error`, of a session, says that it did not quit in time.
fn quit_timed_out(error: &Report<ChromeForTestingError>) -> bool {
    error.iter_reports().any(|report| {
        matches!(
            report.downcast_current_context::<ChromeForTestingError>(),
            Some(ChromeForTestingError::QuitSessionTimeout { .. })
        )
    })
}

/// Bring the page of a new session to the front.
///
/// `ChromeDriver` starts a session on a profile it did not create itself (browser-test names every
/// session's profile, see [`RunProfiles`]) with a page that lacks focus: `document.hasFocus()` is
/// `false`, and focusing an element from script moves `document.activeElement` without firing
/// `focus` or `focusin` events. Sessions on `ChromeDriver`'s own profiles start with a focused
/// page. This restores that behavior.
async fn focus_page(session: &Session) -> WebDriverResult<()> {
    session
        .cdp()
        .send_raw("Page.bringToFront", serde_json::json!({}))
        .await?;
    Ok(())
}

fn configure_chrome_capabilities(
    caps: &mut ChromeCapabilities,
    visible: bool,
    chrome_capabilities_setups: &[Arc<ChromeCapabilitiesSetup>],
) -> WebDriverResult<()> {
    if visible {
        caps.unset_headless()?;
    }
    for setup in chrome_capabilities_setups {
        // A panic would unwind through the whole run: it fails the session instead.
        std::panic::catch_unwind(AssertUnwindSafe(|| setup(caps))).unwrap_or_else(|payload| {
            Err(WebDriverError::SessionCreateError(format!(
                "a Chrome capability setup panicked: {}",
                panic_payload_message(payload.as_ref())
            )))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use assertr::prelude::*;
    use thirtyfour::BrowserCapabilitiesHelper;

    use super::*;

    mod configure_chrome_capabilities {
        use super::*;

        #[test]
        fn applies_visible_mode_before_custom_setup() {
            let custom_setup_called = Arc::new(AtomicUsize::new(0));
            let custom_setup = {
                let custom_setup_called = Arc::clone(&custom_setup_called);
                Arc::new(move |caps: &mut ChromeCapabilities| {
                    assert_that!(caps.is_headless()).is_false();
                    custom_setup_called.fetch_add(1, Ordering::SeqCst);
                    caps.add_arg("--window-size=800,600")
                }) as Arc<ChromeCapabilitiesSetup>
            };
            let mut caps = ChromeCapabilities::new();
            caps.set_headless()
                .expect("setting headless should update capabilities");

            configure_chrome_capabilities(&mut caps, true, &[custom_setup])
                .expect("capability setup should succeed");

            assert_that!(custom_setup_called.load(Ordering::SeqCst)).is_equal_to(1);
            assert_that!(caps.is_headless()).is_false();
            assert_that!(caps.args()).contains("--window-size=800,600");
        }

        #[test]
        fn a_panicking_setup_is_an_error() {
            let panicking = Arc::new(|_: &mut ChromeCapabilities| -> WebDriverResult<()> {
                panic!("no window size")
            }) as Arc<ChromeCapabilitiesSetup>;
            let error =
                configure_chrome_capabilities(&mut ChromeCapabilities::new(), false, &[panicking])
                    .expect_err("a panicking setup should fail");

            assert_that!(error.to_string())
                .contains("a Chrome capability setup panicked: no window size");
        }
    }
}

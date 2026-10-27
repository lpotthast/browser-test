//! Session workers: each creates a browser session, offers it to the pool, runs the tests it is
//! assigned, and quits it.

use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, PoisonError},
    time::{Duration, Instant},
};

use chrome_for_testing_manager::{ChromeForTestingError, Session};
use futures_util::{StreamExt as _, stream::FuturesUnordered};
use rootcause::Report;
use thirtyfour::{
    ChromeCapabilities, ChromiumLikeCapabilities,
    error::{WebDriverError, WebDriverResult},
};
use tokio::sync::{mpsc, oneshot};

use super::{
    ChromeCapabilitiesSetup, Env,
    session_pool::{Delivery, Ticket, WorkerRequest},
    test_run::{JobRun, run_job},
};
use crate::{
    SessionPreparation,
    failure_report::panic_payload_message,
    frozen_pages::close_frozen_pages,
    progress::{Activity, Phase},
    report::FormatDuration,
    session_reuse::{ResetError, SessionBaseline, configure_reusable_session},
};

/// How long resetting a session may take. Its steps usually take about 0.1s.
const RESET_TIME_LIMIT: Duration = Duration::from_secs(30);

/// A test assigned to a session.
pub(super) struct Job {
    pub(super) test: usize,
    /// How long the test waited for its session.
    pub(super) wait: Duration,
    /// Completed once the test body finished, before the session quits or is reset.
    pub(super) body_done: oneshot::Sender<()>,
}

/// Drive the session workers until the pool closed and every worker finished.
pub(super) async fn drive_workers<Context>(
    env: &Env<'_, Context>,
    mut requests: mpsc::UnboundedReceiver<Option<WorkerRequest>>,
) where
    Context: Sync + ?Sized,
{
    let mut workers = FuturesUnordered::new();
    let mut closing = false;
    loop {
        if closing && workers.is_empty() {
            return;
        }
        tokio::select! {
            request = requests.recv(), if !closing => match request {
                Some(Some(request)) => workers.push(session_worker(env, request)),
                Some(None) | None => closing = true,
            },
            Some(()) = workers.next(), if !workers.is_empty() => {}
        }
    }
}

/// Create a session, offer it, run the test it is assigned, and quit it. With
/// [`SessionReuse`](crate::SessionReuse), reset the session after its test and offer it again, as
/// long as upcoming tests need sessions.
async fn session_worker<Context>(env: &Env<'_, Context>, request: WorkerRequest)
where
    Context: Sync + ?Sized,
{
    let pooled = matches!(request.delivery, Delivery::Pool);
    run_session(env, request).await;
    if pooled {
        env.pool.session_ended(env.keep_starting());
    }
}

/// The work of [`session_worker`].
async fn run_session<Context>(env: &Env<'_, Context>, request: WorkerRequest)
where
    Context: Sync + ?Sized,
{
    let config = env.config;
    let activity = Activity::Session(request.session);
    env.board
        .enter(activity, "a browser session", Phase::CreatingSession);
    let creation_start = Instant::now();
    let profile = match config.profiles.create_session_profile() {
        Ok(profile) => profile,
        Err(error) => {
            env.board.clear(activity);
            let report = Report::<rootcause::markers::Dynamic>::from(error)
                .context("the Chrome profile of the session could not be created")
                .into_dynamic();
            env.fail_creation(request.delivery, report, creation_start);
            return;
        }
    };
    let pooled = matches!(request.delivery, Delivery::Pool);
    // Only pool sessions are reused: a dedicated session has settings of its own. With reuse
    // enabled, dedicated sessions are set up like reusable ones all the same, so that their tests
    // see the same browser.
    let reusable = config.session_reuse.is_enabled() && pooled;
    let mut end = SessionEnd {
        delivery: Some(request.delivery),
        last_run: None,
        released: None,
    };
    let session_result = config
        .chrome
        .session()
        .with_caps(|caps: &mut ChromeCapabilities| {
            configure_chrome_capabilities(caps, config.visible, config.chrome_capabilities_setups)?;
            if config.session_reuse.is_enabled() {
                configure_reusable_session(caps, config.session_reuse)?;
            }
            profile.configure(caps)
        })
        .with_cancellation(config.cancellation.clone())
        .with_config(|builder| match request.element_query_wait {
            Some(wait) => builder.poller(Arc::new(wait.into_thirtyfour_poller())),
            None => builder,
        })
        .run(async |session: &Session| {
            serve_tests(
                env,
                session,
                SessionStart {
                    activity,
                    creation_start,
                    pooled,
                    reusable,
                },
                &mut end,
            )
            .await
        })
        .await;
    if let Some(quit_start) = end.released {
        let teardown = quit_start.elapsed();
        env.warn_if_slow_session("Quitting a browser session", teardown);
        *env.released_session_teardown
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += teardown;
    }
    profile.remove().await;
    env.board.clear(activity);

    if let Some(delivery) = end.delivery {
        // The session could not be created, so it was never offered.
        let report = match session_result {
            Err(error) => error.into_dynamic(),
            Ok(()) => rootcause::report!("the browser session ended before it could be used"),
        };
        env.fail_creation(delivery, report, creation_start);
        return;
    }
    let Some(last_run) = end.last_run else {
        // The session quit without a test: no test reports a failure to quit it.
        if let Err(error) = session_result
            && !matches!(error.current_context(), ChromeForTestingError::Cancelled)
        {
            tracing::warn!("Quitting a browser session failed: {error}");
        }
        return;
    };

    let teardown = last_run.quit_start.map(|quit_start| quit_start.elapsed());
    env.board.clear(Activity::Test(last_run.index));
    if let Some(teardown) = teardown {
        let test = env.assigned_test(last_run.index);
        env.warn_if_slow_session(
            &format!("Quitting the session of browser test '{}'", test.name),
            teardown,
        );
    }
    env.finish_job(last_run, session_result, teardown);
}

/// How a worker's session started.
struct SessionStart {
    /// The pool session's activity on the progress board.
    activity: Activity,
    creation_start: Instant,
    /// Whether the session belongs to the pool (rather than to one test with settings of its own).
    pooled: bool,
    /// Whether the session may run further tests after its first one.
    reusable: bool,
}

/// What a session leaves its worker to do once it quit.
struct SessionEnd {
    /// Where the session is offered first. Still set if it never was: it could not be created.
    delivery: Option<Delivery>,
    /// The run of the session's last test, to be recorded with the result of quitting it.
    last_run: Option<JobRun>,
    /// When the session started quitting without a test: no test took it, or the pool released
    /// it after a reset.
    released: Option<Instant>,
}

/// Offer `session` through `end.delivery` (or, once it ran a test, to the pool), and run the tests
/// it is assigned: one, or with [`SessionReuse`](crate::SessionReuse) several, resetting the
/// session in between. What is left to do once the session quit is left in `end`.
async fn serve_tests<Context>(
    env: &Env<'_, Context>,
    session: &Session,
    start: SessionStart,
    end: &mut SessionEnd,
) -> Result<(), Report>
where
    Context: Sync + ?Sized,
{
    let config = env.config;
    let SessionStart {
        activity,
        creation_start,
        pooled,
        reusable,
    } = start;
    if let Err(error) = focus_page(session).await {
        return Err(Report::<rootcause::markers::Dynamic>::from(error)
            .context("the page of the session could not be brought to the front")
            .into_dynamic());
    }
    // Prepared for dedicated sessions as well: it selects where the test runs.
    let mut baseline = if config.session_reuse.is_enabled() {
        match SessionBaseline::prepare(session, config.session_reuse.reset()).await {
            Ok(baseline) => Some(baseline),
            Err(error) => {
                tracing::warn!("A browser session can't be reused, it runs one test only: {error}");
                None
            }
        }
    } else {
        None
    };
    let creation = creation_start.elapsed();
    env.board.clear(activity);
    env.warn_if_slow_session("Creating a browser session", creation);

    let mut preparation = SessionPreparation::Created(creation);
    let mut tests_run = 0;
    loop {
        let (job_tx, job_rx) = oneshot::channel();
        let ticket = Ticket {
            job: job_tx,
            preparation,
        };
        match end.delivery.take() {
            Some(delivery) => env.pool.deliver(delivery, Ok(ticket)),
            // Dropped if the pool has enough sessions: the job channel closes, and the session
            // quits below.
            None => env.pool.deliver_reset(ticket, env.keep_starting()),
        }
        // An error means no test needs this session anymore.
        let Ok(job) = job_rx.await else {
            end.released = Some(Instant::now());
            return Ok(());
        };
        tests_run += 1;
        let mut run = run_job(env, session, &job, preparation).await;
        if pooled {
            env.pool.test_finished();
        }
        // A timed out test may have left commands running in its session.
        let reuse = reusable
            && !run.timed_out()
            && baseline.is_some()
            && config.session_reuse.allows_another_test(tests_run)
            && env.pool.claim_reset(env.keep_starting());
        // Let the next test start while this session quits or is reset.
        let _ = job.body_done.send(());
        let test = env.assigned_test(run.index);
        let subject = format!("browser test '{}'", test.name);
        let Some(baseline) = baseline.as_mut().filter(|_| reuse) else {
            env.board
                .enter(Activity::Test(run.index), subject, Phase::QuittingSession);
            run.quit_start = Some(Instant::now());
            end.last_run = Some(run);
            // The test's own failure is kept in `run`, so that its report doesn't pass
            // through the session.
            return Ok(());
        };

        let index = run.index;
        env.finish_job(run, Ok(()), None);
        env.board
            .enter(Activity::Test(index), subject, Phase::ResettingSession);
        let reset_start = Instant::now();
        let was_reset = reset_session(session, baseline, &test.name).await;
        let reset_duration = reset_start.elapsed();
        env.board.clear(Activity::Test(index));
        env.warn_if_slow_session(
            &format!("Resetting the session of browser test '{}'", test.name),
            reset_duration,
        );
        if !was_reset {
            env.pool.reset_failed(env.keep_starting());
            end.released = Some(Instant::now());
            return Ok(());
        }
        preparation = SessionPreparation::Reset(reset_duration);
    }
}

/// Reset `session` to `baseline` after the browser test `name`, and return whether that worked.
/// If it didn't, the session quits, and why is logged.
async fn reset_session(session: &Session, baseline: &mut SessionBaseline, name: &str) -> bool {
    match tokio::time::timeout(RESET_TIME_LIMIT, baseline.reset(session)).await {
        Ok(Ok(())) => return true,
        Ok(Err(ResetError::DefaultContextUsed(reason))) => tracing::debug!(
            "The session of browser test '{name}' quits instead of being reset: {reason}."
        ),
        Ok(Err(ResetError::Failed(error))) => tracing::warn!(
            "Resetting the session of browser test '{name}' failed, so it quits: {error}"
        ),
        Err(_) => {
            // A page the test left behind may have frozen, which would block quitting as well.
            let pages = close_frozen_pages(session).await;
            tracing::warn!(
                "Resetting the session of browser test '{name}' took longer than {}, so it quits. \
                 {pages}",
                FormatDuration(RESET_TIME_LIMIT),
            );
        }
    }
    false
}

/// Bring the page of a new session to the front.
///
/// `ChromeDriver` starts a session on a profile it did not create itself (browser-test names every
/// session's profile, see [`RunProfiles`](crate::profile::RunProfiles)) with a page that lacks
/// focus: `document.hasFocus()` is `false`, and focusing an element from script moves
/// `document.activeElement` without firing `focus` or `focusin` events. Sessions on
/// `ChromeDriver`'s own profiles start with a focused page. This restores that behavior.
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

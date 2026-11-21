//! Session workers: each creates a browser session, offers it to the pool, runs the tests it is
//! assigned, and quits it.

use std::time::{Duration, Instant};

use chrome_for_testing_manager::ChromeForTestingError;
use futures_util::{StreamExt as _, stream::FuturesUnordered};
use rootcause::Report;
use tokio::sync::{mpsc, oneshot};

use super::{
    Env,
    plan::TestIndex,
    session_pool::{ResetClaim, SessionNumber, Ticket},
    sessions::SessionFactory,
    test_run::{JobRun, run_job},
};
use crate::{
    SessionPreparation,
    progress::{Activity, Phase},
    report::FormatDuration,
    session_reuse::ResetError,
};

/// How long resetting a session, or preparing a new one for resets, may take. Their steps usually
/// take about 0.1s.
pub(super) const RESET_TIME_LIMIT: Duration = Duration::from_secs(30);

/// A test assigned to a session.
pub(super) struct Job {
    pub(super) test: TestIndex,
    /// How long the test waited for its session.
    pub(super) wait: Duration,
    /// Completed once the test body finished, before the session quits or is reset.
    pub(super) body_done: oneshot::Sender<()>,
}

/// Drive the session workers, which get their sessions from `sessions`, until the pool closed and
/// every worker finished. `requests` names the sessions to create, `None` once the pool closed.
pub(super) async fn drive_workers<Context, S>(
    env: &Env<'_, Context>,
    sessions: &S,
    mut requests: mpsc::UnboundedReceiver<Option<SessionNumber>>,
) where
    Context: Sync + ?Sized,
    S: SessionFactory,
{
    let mut workers = FuturesUnordered::new();
    let mut closing = false;
    loop {
        if closing && workers.is_empty() {
            return;
        }
        tokio::select! {
            request = requests.recv(), if !closing => match request {
                Some(Some(session)) => workers.push(session_worker(env, sessions, session)),
                Some(None) | None => closing = true,
            },
            Some(()) = workers.next(), if !workers.is_empty() => {}
        }
    }
}

/// Create a session, offer it, run the test it is assigned, and quit it. With
/// [`SessionReuse`](crate::SessionReuse), reset the session after its test and offer it again, as
/// long as upcoming tests need sessions.
async fn session_worker<Context, S>(env: &Env<'_, Context>, sessions: &S, session: SessionNumber)
where
    Context: Sync + ?Sized,
    S: SessionFactory,
{
    run_session(env, sessions, session).await;
    env.pool.session_ended();
}

/// The work of [`session_worker`].
async fn run_session<Context, S>(env: &Env<'_, Context>, sessions: &S, session: SessionNumber)
where
    Context: Sync + ?Sized,
    S: SessionFactory,
{
    let activity = Activity::Session(session);
    env.board
        .enter(activity, "a browser session", Phase::CreatingSession);
    let creation_start = Instant::now();
    let mut end = SessionEnd {
        offered: false,
        last_run: None,
        released: None,
    };
    let session_result = sessions
        .run_session(async |session: &S::Session| {
            serve_tests(env, sessions, session, activity, creation_start, &mut end).await
        })
        .await;
    if let Some(quit_start) = end.released {
        let teardown = quit_start.elapsed();
        env.warn_if_slow_session("Quitting a browser session", teardown);
        env.results().released_session_teardown += teardown;
    }
    env.board.clear(activity);

    if !end.offered {
        // The session could not be created.
        let report = match session_result {
            Err(error) => error,
            Ok(()) => rootcause::report!("the browser session ended before it could be used"),
        };
        env.fail_creation(report, creation_start);
        return;
    }
    let Some(last_run) = end.last_run else {
        // The session quit without a test: no test reports a failure to quit it.
        if let Err(error) = session_result
            && !matches!(
                error.downcast_current_context::<ChromeForTestingError>(),
                Some(ChromeForTestingError::Cancelled)
            )
        {
            tracing::warn!("Quitting a browser session failed: {error}");
        }
        return;
    };

    let teardown = last_run.quit_start.map(|quit_start| quit_start.elapsed());
    env.board.clear(Activity::Test(last_run.index));
    if let Some(teardown) = teardown {
        let test = env.test(last_run.index);
        env.warn_if_slow_session(
            &format!("Quitting the session of browser test '{}'", test.name),
            teardown,
        );
    }
    last_run.finish(env, session_result, teardown);
}

/// What a session leaves its worker to do once it quit.
struct SessionEnd {
    /// Whether the session was offered to the pool. It wasn't if it could not be created.
    offered: bool,
    /// The run of the session's last test, to be recorded with the result of quitting it.
    last_run: Option<JobRun>,
    /// When the session started quitting without a test: no test took it, or the pool released
    /// it after a reset.
    released: Option<Instant>,
}

/// Offer `session`, created since `creation_start`, to the pool, and run the tests it is assigned:
/// one, or with [`SessionReuse`](crate::SessionReuse) several, resetting the session in between.
/// `activity` is the session's on the progress board until it is offered. What is left to do once
/// the session quit is left in `end`.
async fn serve_tests<Context, S>(
    env: &Env<'_, Context>,
    sessions: &S,
    session: &S::Session,
    activity: Activity,
    creation_start: Instant,
    end: &mut SessionEnd,
) -> Result<(), Report>
where
    Context: Sync + ?Sized,
    S: SessionFactory,
{
    let config = &env.config;
    sessions.ready(session).await?;
    let mut baseline = if config.session_reuse.is_enabled() {
        let prepare = sessions.baseline(session, config.session_reuse.reset());
        match tokio::time::timeout(RESET_TIME_LIMIT, prepare).await {
            Ok(Ok(baseline)) => Some(baseline),
            Ok(Err(error)) => {
                tracing::warn!("A browser session can't be reused, it runs one test only: {error}");
                None
            }
            Err(_) => {
                tracing::warn!(
                    "Preparing a browser session for reuse took longer than {}, it runs one test \
                     only.",
                    FormatDuration(RESET_TIME_LIMIT),
                );
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
    // The claimed reset of the session after its last test.
    let mut reset: Option<ResetClaim<'_>> = None;
    loop {
        let (job_tx, job_rx) = oneshot::channel();
        let ticket = Ticket {
            job: job_tx,
            preparation,
        };
        if !end.offered {
            end.offered = true;
            env.pool.deliver(Ok(ticket));
        } else if let Some(reset) = reset.take() {
            // Dropped if the pool has enough sessions: the job channel closes, and the session
            // quits below.
            reset.deliver(ticket);
        }
        // An error means no test needs this session anymore.
        let Ok(job) = job_rx.await else {
            end.released = Some(Instant::now());
            return Ok(());
        };
        tests_run += 1;
        let mut run = run_job(env, sessions, session, &job, preparation).await;
        env.pool.test_finished();
        // A timed out test may have left commands running in its session.
        let claim = if !run.timed_out() && config.session_reuse.allows_another_test(tests_run) {
            baseline
                .as_mut()
                .and_then(|baseline| Some((baseline, env.pool.claim_reset()?)))
        } else {
            None
        };
        // Let the next test start while this session quits or is reset.
        let _ = job.body_done.send(());
        let test = env.test(run.index);
        let subject = format!("browser test '{}'", test.name);
        let Some((baseline, claim)) = claim else {
            env.board
                .enter(Activity::Test(run.index), subject, Phase::QuittingSession);
            run.quit_start = Some(Instant::now());
            end.last_run = Some(run);
            // The test's own failure is kept in `run`, so that its report doesn't pass
            // through the session.
            return Ok(());
        };

        let index = run.index;
        run.finish(env, Ok(()), None);
        env.board
            .enter(Activity::Test(index), subject, Phase::ResettingSession);
        let reset_start = Instant::now();
        let was_reset = reset_session(sessions, session, baseline, &test.name).await;
        let reset_duration = reset_start.elapsed();
        env.board.clear(Activity::Test(index));
        env.warn_if_slow_session(
            &format!("Resetting the session of browser test '{}'", test.name),
            reset_duration,
        );
        if !was_reset {
            // Dropping the claim lets the pool create another session if tests need it.
            drop(claim);
            end.released = Some(Instant::now());
            return Ok(());
        }
        reset = Some(claim);
        preparation = SessionPreparation::Reset(reset_duration);
    }
}

/// Reset `session` to `baseline` after the browser test `name`, and return whether that worked.
/// If it didn't, the session quits, and why is logged.
async fn reset_session<S>(
    sessions: &S,
    session: &S::Session,
    baseline: &mut S::Baseline,
    name: &str,
) -> bool
where
    S: SessionFactory,
{
    match tokio::time::timeout(RESET_TIME_LIMIT, sessions.reset(session, baseline)).await {
        Ok(Ok(())) => return true,
        Ok(Err(ResetError::DefaultContextUsed(reason))) => tracing::debug!(
            "The session of browser test '{name}' quits instead of being reset: {reason}."
        ),
        Ok(Err(ResetError::Failed(error))) => tracing::warn!(
            "Resetting the session of browser test '{name}' failed, so it quits: {error}"
        ),
        Err(_) => {
            // A page the test left behind may have frozen, which would block quitting as well.
            let pages = sessions.close_frozen_pages(session).await;
            tracing::warn!(
                "Resetting the session of browser test '{name}' took longer than {}, so it quits. \
                 {pages}",
                FormatDuration(RESET_TIME_LIMIT),
            );
        }
    }
    false
}

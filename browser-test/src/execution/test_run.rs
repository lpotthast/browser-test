//! Running a test in its session, and the outcome of the run with its failure report.

use std::{
    collections::BTreeMap,
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    time::{Duration, Instant},
};

use futures_util::FutureExt as _;
use rootcause::Report;
use tracing::Instrument as _;

use super::{Env, plan::QueuedTest, sessions::SessionFactory, worker::Job};
use crate::{
    BrowserTestError,
    driver_output::{DriverOutputSnapshot, attach_session_output},
    failure_report::{
        PanicDetails, PanicLocation, RecentSteps, TestCodeFramesHandler, panic_payload_message,
        without_own_location,
    },
    frozen_pages::PageCheck,
    progress::Activity,
    report::{SessionPreparation, SessionTiming, StepStats, TestOutcome, TestRecord},
    step::StepRecorder,
};

/// What happened while a session ran its test.
pub(super) struct JobRun {
    pub(super) index: usize,
    preparation: SessionPreparation,
    wait: Duration,
    body: Option<Duration>,
    steps: BTreeMap<String, StepStats>,
    /// The test's last steps, for its failure report.
    recent_steps: Option<RecentSteps>,
    /// How the test failed, if it did.
    failure: Option<Failure>,
    /// Where the driver's output of the test starts, and the id of its session, if captured.
    driver_output: Option<(usize, String)>,
    pub(super) quit_start: Option<Instant>,
}

impl JobRun {
    /// Whether the test body ran longer than its time limit. It may have left commands running in
    /// its session.
    pub(super) fn timed_out(&self) -> bool {
        matches!(self.failure, Some(Failure::TimedOut { .. }))
    }
}

/// How a test failed.
enum Failure {
    /// Its session could not be prepared for it, so it didn't run.
    PrepareSession(Report),
    /// It returned an error.
    Error(Report),
    /// It panicked, with this message. `details` says where, if the panic hook saw it.
    Panic {
        message: String,
        details: Option<PanicDetails>,
    },
    /// It ran longer than its time limit. `pages` is what the check of its pages found, and
    /// `drop_panic` the message and details of a panic while it was stopped, if it panicked.
    TimedOut {
        limit: Duration,
        pages: PageCheck,
        drop_panic: Option<(String, Option<PanicDetails>)>,
    },
}

impl JobRun {
    /// Record the finished test of this run in `env`. Its session ended with `session_result`
    /// after `teardown` (`None`: the session runs further tests).
    pub(super) fn finish<Context>(
        self,
        env: &Env<'_, Context>,
        session_result: Result<(), Report>,
        teardown: Option<Duration>,
    ) where
        Context: Sync + ?Sized,
    {
        let test = env.test(self.index);
        // Only now: the output of quitting the session is included, and caught up with.
        let failed = self.failure.is_some() || session_result.is_err();
        let driver_output = self
            .driver_output
            .filter(|_| failed)
            .zip(env.config.driver_output)
            .and_then(|((start, session_id), capture)| capture.session_output(start, &session_id));
        let (outcome, result) = test_outcome(
            &test.name,
            self.failure,
            self.recent_steps,
            driver_output,
            session_result,
        );
        let session = SessionTiming {
            preparation: self.preparation,
            wait: self.wait,
        };
        let record = TestRecord {
            teardown,
            steps: self.steps,
            ..test.record(outcome, Some(session), self.body)
        };
        env.finish(record, result);
    }
}

/// Run the test of `job` in `session` of `sessions`, prepared as `preparation`.
pub(super) async fn run_job<Context, S>(
    env: &Env<'_, Context>,
    sessions: &S,
    session: &S::Session,
    job: &Job,
    preparation: SessionPreparation,
) -> JobRun
where
    Context: Sync + ?Sized,
    S: SessionFactory,
{
    let test = env.test(job.test);
    let mut run = JobRun {
        index: job.test,
        preparation,
        wait: job.wait,
        body: None,
        steps: BTreeMap::new(),
        recent_steps: None,
        failure: None,
        driver_output: env
            .config
            .driver_output
            .map(|capture| (capture.position(), sessions.session_id(session))),
        quit_start: None,
    };
    let timeouts = test.ready().timeouts;
    let timeouts = if timeouts.is_empty() {
        Ok(())
    } else {
        sessions.set_timeouts(session, timeouts).await
    };
    match timeouts {
        Ok(()) => run_body(env, sessions, session, test, &mut run).await,
        Err(error) => {
            // Don't let further tests start while this session quits.
            env.stop_starting_on_fail_fast();
            run.failure = Some(Failure::PrepareSession(error));
        }
    }
    run
}

/// The outcome of a test that ended with `failure`, if any, and whose session ended with
/// `session_result`. A failure's report says where the test failed
/// ([`TestCodeFrames`](crate::failure_report::TestCodeFrames)), what it did before
/// (`recent_steps`), and what its session's driver printed meanwhile (`driver_output`).
///
/// An error of `session_result` comes from quitting the session after the test: a failed test
/// keeps it as a child of its report, and a test that passed fails with it.
fn test_outcome(
    name: &str,
    failure: Option<Failure>,
    recent_steps: Option<RecentSteps>,
    driver_output: Option<DriverOutputSnapshot>,
    session_result: Result<(), Report>,
) -> (TestOutcome, Result<(), Report<BrowserTestError>>) {
    let quit_error = session_result.err();
    let (outcome, report) = match (failure, quit_error) {
        (Some(failure), None) => failure_report(name, failure),
        (Some(failure), Some(quit_error)) => {
            let (outcome, mut report) = failure_report(name, failure);
            report.children_mut().push(quit_error.into_cloneable());
            (outcome, report)
        }
        (None, Some(quit_error)) => (
            TestOutcome::Failed,
            without_own_location(quit_error.context(BrowserTestError::RunTest {
                test_name: name.to_owned(),
            })),
        ),
        (None, None) => return (TestOutcome::Passed, Ok(())),
    };
    let report = match recent_steps {
        Some(steps) => report.attach_custom::<rootcause::handlers::Display, _>(steps),
        None => report,
    };
    (outcome, Err(attach_session_output(report, driver_output)))
}

/// The outcome of the test `name` that failed with `failure`, and its report.
fn failure_report(name: &str, failure: Failure) -> (TestOutcome, Report<BrowserTestError>) {
    let test_name = name.to_owned();
    match failure {
        Failure::PrepareSession(error) => (
            TestOutcome::Failed,
            without_own_location(error.context(BrowserTestError::PrepareSession { test_name })),
        ),
        Failure::Error(error) => (
            TestOutcome::Failed,
            without_own_location(error.context(BrowserTestError::RunTest { test_name })),
        ),
        Failure::Panic { message, details } => (
            TestOutcome::Panicked,
            panic_report(test_name, message, details),
        ),
        Failure::TimedOut {
            limit,
            pages,
            drop_panic,
        } => {
            let mut report = without_own_location(Report::new(BrowserTestError::TimedOut {
                test_name: test_name.clone(),
                limit,
            }))
            .attach_custom::<rootcause::handlers::Display, _>(pages);
            // A panic while the test was stopped is a failure of its own.
            if let Some((message, details)) = drop_panic {
                report.children_mut().push(
                    panic_report(test_name, message, details)
                        .into_dynamic()
                        .into_cloneable(),
                );
            }
            (TestOutcome::TimedOut, report)
        }
    }
}

/// The report of the test `test_name` that panicked with `message`, where `details` say.
fn panic_report(
    test_name: String,
    message: String,
    details: Option<PanicDetails>,
) -> Report<BrowserTestError> {
    let mut report =
        without_own_location(Report::new(BrowserTestError::Panic { test_name, message }));
    if let Some(PanicDetails { location, frames }) = details {
        if let Some(location) = location {
            report = report.attach_custom::<rootcause::handlers::Display, _>(PanicLocation {
                location,
                frames: frames.clone(),
            });
        }
        if let Some(frames) = frames {
            report = report.attach_custom::<TestCodeFramesHandler, _>(frames);
        }
    }
    report
}

/// Run the body of `test`, recording its duration, steps and failure into `run`. A body exceeding
/// the time limit has its session's frozen pages closed.
async fn run_body<Context, S>(
    env: &Env<'_, Context>,
    sessions: &S,
    session: &S::Session,
    test: &QueuedTest<Context>,
    run: &mut JobRun,
) where
    Context: Sync + ?Sized,
    S: SessionFactory,
{
    let name = test.name.as_str();
    tracing::debug!("Executing browser test: {name}");
    let recorder = StepRecorder::new(env.config.progress_warnings.slow_step());
    env.board.enter_running_test(
        Activity::Test(test.index),
        format!("browser test '{name}'"),
        &recorder,
    );
    let span = tracing::info_span!(
        "browser_test",
        test = %name,
        path = test.placement.path.as_deref().unwrap_or_default(),
        tags = %test.tags.join(", "),
    );
    let start = Instant::now();
    // `run` is called in the block, so that a panic before it returns its future (in a
    // hand-written implementation) is caught as well.
    let prepared = test.ready();
    let body = async {
        sessions
            .run_test(session, &*prepared.test, env.context)
            .await
    };
    let mut body = Body::new(
        recorder
            .scope(AssertUnwindSafe(body).catch_unwind())
            .instrument(span),
    );
    // The timeout only drops `run`, which borrows the body: `close` drops the body itself.
    let result = tokio::time::timeout(prepared.time_limit, body.run())
        .await
        .map_err(|_elapsed| ());
    // Before its steps are taken, as steps it drops unfinished record themselves.
    let drop_panic = body.close().err();
    run.body = Some(start.elapsed());
    run.steps = recorder.take();
    run.recent_steps = recorder.recent_steps();
    if !matches!(result, Ok(Ok(Ok(())))) {
        // Don't let further tests start while its pages are checked or its session quits.
        env.stop_starting_on_fail_fast();
    }
    run.failure = match result {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(error))) => Some(Failure::Error(error)),
        Ok(Err(payload)) => {
            let message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{name}' panicked: {message}");
            Some(Failure::Panic {
                message,
                details: recorder.take_panic(),
            })
        }
        Err(()) => {
            tracing::error!("Browser test '{name}' exceeded its time limit, checking its pages.");
            let drop_panic = drop_panic.map(|payload| {
                let message = panic_payload_message(payload.as_ref());
                tracing::error!("Browser test '{name}' panicked while it was stopped: {message}");
                (message, recorder.take_panic())
            });
            Some(Failure::TimedOut {
                limit: prepared.time_limit,
                pages: sessions.close_frozen_pages(session).await,
                drop_panic,
            })
        }
    };
    if run.failure.is_some()
        && let Some(capture) = env.config.driver_output
    {
        capture.catch_up().await;
    }
}

/// A test body that catches panics while it is dropped as well, which `catch_unwind` only does
/// while it is polled. Dropping a body that didn't finish (it timed out, or the run was cancelled)
/// drops what it holds, whose `Drop` implementations may panic.
struct Body<F> {
    future: Option<Pin<Box<F>>>,
}

impl<F: Future> Body<F> {
    fn new(future: F) -> Self {
        Self {
            future: Some(Box::pin(future)),
        }
    }

    /// Run the body until it finishes.
    async fn run(&mut self) -> F::Output {
        match &mut self.future {
            Some(future) => future.as_mut().await,
            None => unreachable!("a body is only closed after it ran"),
        }
    }
}

impl<F> Body<F> {
    /// Drop the body. `Err` holds the payload of a panic while dropping it.
    fn close(mut self) -> std::thread::Result<()> {
        let future = self.future.take();
        std::panic::catch_unwind(AssertUnwindSafe(|| drop(future)))
    }
}

impl<F> Drop for Body<F> {
    /// Drop a body that wasn't closed: its run was cancelled, so a panic can only be logged.
    fn drop(&mut self) {
        let future = self.future.take();
        if let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(|| drop(future))) {
            tracing::error!(
                "A cancelled browser test panicked while it was stopped: {}",
                panic_payload_message(payload.as_ref())
            );
        }
    }
}

//! Running a test in its session, and the outcome of the run with its failure report.

use std::{
    collections::BTreeMap,
    panic::AssertUnwindSafe,
    time::{Duration, Instant},
};

use chrome_for_testing_manager::{ChromeForTestingError, Session};
use futures_util::FutureExt as _;
use rootcause::{Report, markers::Dynamic};
use tracing::Instrument as _;

use super::{Env, plan::PreparedTest, worker::Job};
use crate::{
    BrowserTestError,
    failure_report::{
        PanicDetails, PanicLocation, RecentSteps, panic_payload_message, without_own_location,
    },
    frozen_pages::{PageCheck, close_frozen_pages},
    progress::{Activity, Phase},
    report::{BrowserTestRecord, SessionPreparation, SessionTiming, StepStats, TestOutcome},
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
    /// It ran longer than its time limit. `pages` is what the check of its pages found.
    TimedOut { limit: Duration, pages: PageCheck },
}

impl<Context, TestError> Env<'_, Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    /// Record the finished test of `run`, whose session ended with `session_result` after
    /// `teardown` (`None`: the session runs further tests).
    pub(super) fn finish_job(
        &self,
        run: JobRun,
        session_result: Result<(), Report<ChromeForTestingError>>,
        teardown: Option<Duration>,
    ) {
        let test = self.assigned_test(run.index);
        let (outcome, result) =
            test_outcome(&test.name, run.failure, run.recent_steps, session_result);
        let record = BrowserTestRecord {
            index: run.index,
            name: test.name.clone(),
            outcome,
            path: test.placement.path.clone(),
            group: test.placement.group.clone(),
            session: Some(SessionTiming {
                preparation: run.preparation,
                wait: run.wait,
            }),
            body: run.body,
            teardown,
            steps: run.steps,
        };
        self.finish(record, result);
    }
}

/// Run the test of `job` in `session`, prepared as `preparation`.
pub(super) async fn run_job<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    session: &Session,
    job: &Job,
    preparation: SessionPreparation,
) -> JobRun
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let test = env.assigned_test(job.test);
    let mut run = JobRun {
        index: job.test,
        preparation,
        wait: job.wait,
        body: None,
        steps: BTreeMap::new(),
        recent_steps: None,
        failure: None,
        quit_start: None,
    };
    let timeouts = if test.timeouts.is_empty() {
        Ok(())
    } else {
        session
            .update_timeouts(test.timeouts.into_thirtyfour_timeout_configuration())
            .await
    };
    match timeouts {
        Ok(()) => run_body(env, session, test, &mut run).await,
        Err(error) => {
            // Don't let further tests start while this session quits.
            env.stop_starting_on_fail_fast();
            run.failure = Some(Failure::PrepareSession(Report::<Dynamic>::from(error)));
        }
    }
    run
}

/// The outcome of a test that ended with `failure`, if any, and whose session ended with
/// `session_result`. A failure's report says where the test failed
/// ([`TestCodeFrames`](crate::failure_report::TestCodeFrames)) and what it did before
/// (`recent_steps`).
///
/// An error of `session_result` comes from quitting the session after the test: a failed test
/// keeps it as a child of its report, and a test that passed fails with it.
fn test_outcome(
    name: &str,
    failure: Option<Failure>,
    recent_steps: Option<RecentSteps>,
    session_result: Result<(), Report<ChromeForTestingError>>,
) -> (TestOutcome, Result<(), Report<BrowserTestError>>) {
    let quit_error = session_result.err().map(Report::into_dynamic);
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
    (outcome, Err(report))
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
        Failure::Panic { message, details } => {
            let mut report =
                without_own_location(Report::new(BrowserTestError::Panic { test_name, message }));
            if let Some(PanicDetails { location, frames }) = details {
                if let Some(location) = location {
                    let outside_test_code = frames
                        .as_ref()
                        .is_some_and(|frames| !frames.starts_at(&location));
                    report =
                        report.attach_custom::<rootcause::handlers::Display, _>(PanicLocation {
                            location,
                            outside_test_code,
                        });
                }
                if let Some(frames) = frames {
                    report = report.attach_custom::<rootcause::handlers::Display, _>(frames);
                }
            }
            (TestOutcome::Panicked, report)
        }
        Failure::TimedOut { limit, pages } => (
            TestOutcome::TimedOut,
            without_own_location(Report::new(BrowserTestError::TimedOut { test_name, limit }))
                .attach_custom::<rootcause::handlers::Display, _>(pages),
        ),
    }
}

/// Run the body of `test`, recording its duration, steps and failure into `run`. A body exceeding
/// the time limit has its session's frozen pages closed.
async fn run_body<Context, TestError>(
    env: &Env<'_, Context, TestError>,
    session: &Session,
    test: &PreparedTest<Context, TestError>,
    run: &mut JobRun,
) where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    let name = test.name.as_str();
    tracing::debug!("Executing browser test: {name}");
    env.board.enter(
        Activity::Test(test.index),
        format!("browser test '{name}'"),
        Phase::RunningTest,
    );
    let recorder = StepRecorder::new(env.config.progress_warnings.slow_step());
    let span = tracing::info_span!(
        "browser_test",
        test = %name,
        path = test.placement.path.as_deref().unwrap_or_default(),
        group = test.placement.group.as_deref().unwrap_or_default(),
    );
    let start = Instant::now();
    // `run` is called in the block, so that a panic before it returns its future (in a
    // hand-written implementation) is caught as well.
    let body = async { test.test.run(session, env.context).await };
    let body = recorder
        .scope(AssertUnwindSafe(body).catch_unwind())
        .instrument(span);
    let result = tokio::time::timeout(test.time_limit, body).await;
    run.body = Some(start.elapsed());
    run.steps = recorder.take();
    run.recent_steps = recorder.recent_steps();
    if !matches!(result, Ok(Ok(Ok(())))) {
        // Don't let further tests start while its pages are checked or its session quits.
        env.stop_starting_on_fail_fast();
    }
    run.failure = match result {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(error))) => Some(Failure::Error(error.into_dynamic())),
        Ok(Err(payload)) => {
            let message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{name}' panicked: {message}");
            Some(Failure::Panic {
                message,
                details: recorder.take_panic(),
            })
        }
        Err(_elapsed) => {
            tracing::error!("Browser test '{name}' exceeded its time limit, checking its pages.");
            Some(Failure::TimedOut {
                limit: test.time_limit,
                pages: close_frozen_pages(session).await,
            })
        }
    };
}

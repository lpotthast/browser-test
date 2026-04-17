use std::{future::Future, num::NonZeroUsize, pin::Pin};

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use rootcause::Report;
use rootcause::report_collection::ReportCollection;

use crate::BrowserTestError;

/// How [`crate::BrowserTestRunner`] schedules browser tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserTestParallelism {
    /// Run one test at a time.
    #[default]
    Sequential,

    /// Run up to the given number of tests at the same time.
    ///
    /// Each test still receives a fresh `WebDriver` session.
    ///
    /// Using `1` here leads to the same behavior as using `Sequential`.
    Parallel(NonZeroUsize),
}

impl BrowserTestParallelism {
    pub(crate) const fn max_parallel_tests(self) -> NonZeroUsize {
        match self {
            Self::Sequential => NonZeroUsize::MIN,
            Self::Parallel(max_parallel_tests) => max_parallel_tests,
        }
    }
}

/// How [`crate::BrowserTestRunner`] handles failed browser tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserTestFailurePolicy {
    /// Stop after the first failed test.
    ///
    /// When tests are running in parallel, the runner stops starting additional tests and waits for
    /// already-started sessions to finish before reporting failures from those sessions.
    #[default]
    FailFast,

    /// Run every test and return all failures as child reports on one aggregate report.
    RunAll,
}

pub(crate) struct BrowserTestExecution {
    pub(crate) test_index: usize,
    pub(crate) result: Result<(), Report<BrowserTestError>>,
}

pub(crate) type BrowserTestExecutionFuture<'a> =
    Pin<Box<dyn Future<Output = BrowserTestExecution> + Send + 'a>>;

pub(crate) async fn run_test_executions_sequential<'a>(
    failure_policy: BrowserTestFailurePolicy,
    executions: impl IntoIterator<Item = BrowserTestExecutionFuture<'a>>,
) -> Result<(), Report<BrowserTestError>> {
    let mut failures = BrowserTestFailures::default();

    for execution in executions {
        let execution = execution.await;
        match execution.result {
            Ok(()) => {}
            Err(err) => {
                if failure_policy == BrowserTestFailurePolicy::FailFast {
                    return Err(err);
                }
                failures.push(execution.test_index, err);
            }
        }
    }

    failures.into_result()
}

pub(crate) async fn run_test_executions_parallel<'a>(
    failure_policy: BrowserTestFailurePolicy,
    executions: impl IntoIterator<Item = BrowserTestExecutionFuture<'a>>,
    max_parallel_tests: NonZeroUsize,
) -> Result<(), Report<BrowserTestError>> {
    let mut tests = executions.into_iter();
    let mut running = FuturesUnordered::new();
    let mut keep_starting = true;
    let mut collected_failures = BrowserTestFailures::default();

    // Start up to `max_parallel_tests` initially.
    while running.len() < max_parallel_tests.get() {
        match tests.next() {
            None => break,
            Some(test) => running.push(test),
        }
    }

    // Keep starting an additional test once any previous test completes, keeping us topped up at
    // `max_parallel_tests` until all tests are executing and completing.
    while let Some(execution) = running.next().await {
        if let Err(err) = execution.result {
            if failure_policy == BrowserTestFailurePolicy::FailFast {
                keep_starting = false;
            }
            collected_failures.push(execution.test_index, err);
        }

        if keep_starting && let Some(test) = tests.next() {
            running.push(test);
        }
    }

    collected_failures.into_result()
}

#[derive(Default)]
struct BrowserTestFailures {
    failures: Vec<(usize, Report<BrowserTestError>)>,
}

impl BrowserTestFailures {
    fn push(&mut self, test_index: usize, failure: Report<BrowserTestError>) {
        self.failures.push((test_index, failure));
    }

    fn into_result(mut self) -> Result<(), Report<BrowserTestError>> {
        if self.failures.is_empty() {
            return Ok(());
        }

        let failed_tests = self.failures.len();
        self.failures
            .sort_by_key(|(test_index, _failure)| *test_index);

        let mut failure_collection = ReportCollection::with_capacity(failed_tests);
        for (_test_index, failure) in self.failures {
            failure_collection.push(failure.into_cloneable());
        }

        Err(failure_collection.context(BrowserTestError::RunTests { failed_tests }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use assertr::prelude::*;

    use super::*;

    #[test]
    fn parallelism_max_parallel_tests_treats_sequential_as_one() {
        assert_that!(
            BrowserTestParallelism::Sequential
                .max_parallel_tests()
                .get()
        )
        .is_equal_to(1);

        let max_parallel_tests =
            NonZeroUsize::new(3).expect("literal parallelism should be non-zero");
        assert_that!(
            BrowserTestParallelism::Parallel(max_parallel_tests)
                .max_parallel_tests()
                .get()
        )
        .is_equal_to(3);
    }

    #[test]
    fn browser_test_failures_returns_ok_when_empty() {
        let failures = BrowserTestFailures::default();

        assert_that!(failures.into_result()).is_ok();
    }

    #[test]
    fn browser_test_failures_returns_aggregate_report_with_children() {
        let mut failures = BrowserTestFailures::default();
        failures.push(
            0,
            Report::new(BrowserTestError::RunTest {
                test_name: "login".to_owned(),
            }),
        );
        failures.push(
            1,
            Report::new(BrowserTestError::RunTest {
                test_name: "checkout".to_owned(),
            }),
        );

        let err = failures
            .into_result()
            .expect_err("non-empty failure collection should fail");

        assert_that!(err.to_string())
            .contains(BrowserTestError::RunTests { failed_tests: 2 }.to_string());
        assert_that!(err.children().len()).is_equal_to(2);
    }

    #[test]
    fn sequential_fail_fast_stops_after_first_failure() {
        let runtime = current_thread_runtime();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));

        let result = runtime.block_on(run_test_executions_sequential(
            BrowserTestFailurePolicy::FailFast,
            [
                tracked_execution(first.clone(), failing_execution(0, "first")),
                tracked_execution(second.clone(), passing_execution(1)),
            ],
        ));

        assert_that!(result).is_err();
        assert_that!(first.load(Ordering::SeqCst)).is_equal_to(1);
        assert_that!(second.load(Ordering::SeqCst)).is_equal_to(0);
    }

    #[test]
    fn sequential_run_all_continues_after_failure_and_panic() {
        let runtime = current_thread_runtime();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let third = Arc::new(AtomicUsize::new(0));

        let err = runtime
            .block_on(run_test_executions_sequential(
                BrowserTestFailurePolicy::RunAll,
                [
                    tracked_execution(first.clone(), failing_execution(0, "first")),
                    tracked_execution(second.clone(), panicked_execution(1, "second")),
                    tracked_execution(third.clone(), passing_execution(2)),
                ],
            ))
            .expect_err("run-all should report collected failures");

        assert_that!(first.load(Ordering::SeqCst)).is_equal_to(1);
        assert_that!(second.load(Ordering::SeqCst)).is_equal_to(1);
        assert_that!(third.load(Ordering::SeqCst)).is_equal_to(1);
        assert_that!(err.children().len()).is_equal_to(2);
        assert_that!(err.to_string())
            .contains(BrowserTestError::RunTests { failed_tests: 2 }.to_string());
    }

    #[test]
    fn parallel_fail_fast_stops_starting_new_tests_but_waits_for_running_tests() {
        let runtime = current_thread_runtime();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let third = Arc::new(AtomicUsize::new(0));

        let err = runtime
            .block_on(run_test_executions_parallel(
                BrowserTestFailurePolicy::FailFast,
                [
                    tracked_execution(first.clone(), failing_execution(0, "first")),
                    tracked_execution(second.clone(), failing_execution(1, "second")),
                    tracked_execution(third.clone(), passing_execution(2)),
                ],
                NonZeroUsize::new(2).expect("literal parallelism should be non-zero"),
            ))
            .expect_err("fail-fast should report failures from already-running tests");

        assert_that!(err.children().len()).is_equal_to(2);
        assert_that!(first.load(Ordering::SeqCst)).is_equal_to(1);
        assert_that!(second.load(Ordering::SeqCst)).is_equal_to(1);
        assert_that!(third.load(Ordering::SeqCst)).is_equal_to(0);
    }

    #[test]
    fn parallel_run_all_starts_every_test() {
        let runtime = current_thread_runtime();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let third = Arc::new(AtomicUsize::new(0));

        runtime
            .block_on(run_test_executions_parallel(
                BrowserTestFailurePolicy::RunAll,
                [
                    tracked_execution(first.clone(), passing_execution(0)),
                    tracked_execution(second.clone(), passing_execution(1)),
                    tracked_execution(third.clone(), passing_execution(2)),
                ],
                NonZeroUsize::new(2).expect("literal parallelism should be non-zero"),
            ))
            .expect("all passing executions should succeed");

        assert_that!(first.load(Ordering::SeqCst)).is_equal_to(1);
        assert_that!(second.load(Ordering::SeqCst)).is_equal_to(1);
        assert_that!(third.load(Ordering::SeqCst)).is_equal_to(1);
    }

    fn current_thread_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime should build")
    }

    fn tracked_execution(
        counter: Arc<AtomicUsize>,
        execution: BrowserTestExecution,
    ) -> BrowserTestExecutionFuture<'static> {
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            execution
        })
    }

    fn passing_execution(test_index: usize) -> BrowserTestExecution {
        BrowserTestExecution {
            test_index,
            result: Ok(()),
        }
    }

    fn failing_execution(test_index: usize, test_name: &str) -> BrowserTestExecution {
        BrowserTestExecution {
            test_index,
            result: Err(Report::new(BrowserTestError::RunTest {
                test_name: test_name.to_owned(),
            })),
        }
    }

    fn panicked_execution(test_index: usize, test_name: &str) -> BrowserTestExecution {
        BrowserTestExecution {
            test_index,
            result: Err(Report::new(BrowserTestError::Panic {
                test_name: test_name.to_owned(),
                message: "boom".to_owned(),
            })),
        }
    }
}

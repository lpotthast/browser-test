use std::num::NonZeroUsize;

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
    /// Every slot creates its own sessions. With session reuse, each slot reuses its session for
    /// the tests it runs one after another.
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

#[derive(Default)]
pub(crate) struct BrowserTestFailures {
    failures: Vec<(usize, Report<BrowserTestError>)>,
}

impl BrowserTestFailures {
    pub(crate) fn push(&mut self, test_index: usize, failure: Report<BrowserTestError>) {
        self.failures.push((test_index, failure));
    }

    /// The first failure as is, for sequential fail-fast runs, which stop after it.
    pub(crate) fn into_first_result(mut self) -> Result<(), Report<BrowserTestError>> {
        if self.failures.len() > 1 {
            return self.into_result();
        }
        match self.failures.pop() {
            None => Ok(()),
            Some((_test_index, failure)) => Err(failure),
        }
    }

    pub(crate) fn into_result(mut self) -> Result<(), Report<BrowserTestError>> {
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
    fn browser_test_failures_first_result_returns_single_failure_unwrapped() {
        let mut failures = BrowserTestFailures::default();
        failures.push(
            0,
            Report::new(BrowserTestError::RunTest {
                test_name: "login".to_owned(),
            }),
        );

        let err = failures
            .into_first_result()
            .expect_err("a failure should fail");

        assert_that!(err.current_context().clone()).is_equal_to(BrowserTestError::RunTest {
            test_name: "login".to_owned(),
        });
        assert_that!(err.children().len()).is_equal_to(0);
    }

    #[test]
    fn browser_test_failures_first_result_is_ok_when_empty() {
        assert_that!(BrowserTestFailures::default().into_first_result()).is_ok();
    }
}

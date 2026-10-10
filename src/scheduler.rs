use std::num::NonZeroUsize;

use rootcause::{Report, report_collection::ReportCollection};

use crate::{
    BrowserTestError,
    env::{InvalidEnvVar, env_number},
};

/// Default environment variable read by [`Parallelism::from_env`].
pub(crate) const DEFAULT_PARALLELISM_ENV: &str = "BROWSER_TEST_PARALLELISM";

/// How many entries of a [`crate::BrowserTests`] group run at the same time, each test in its own
/// session.
///
/// Sequential by default. Tests take parallel slots in their given order. A test whose browser is
/// already running may start before an earlier test whose browser is still starting. Only run
/// tests in parallel that can use the same application state at the same time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Parallelism {
    /// `None` runs sequentially.
    max_parallel_tests: Option<NonZeroUsize>,
}

impl Parallelism {
    /// Run one test at a time.
    #[must_use]
    pub const fn sequential() -> Self {
        Self {
            max_parallel_tests: None,
        }
    }

    /// Run up to `max_parallel_tests` tests at the same time. `0` and `1` run tests sequentially.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use browser_test::{BrowserTests, Parallelism};
    ///
    /// let tests = BrowserTests::<()>::parallel(Parallelism::parallel(4));
    /// ```
    #[must_use]
    pub const fn parallel(max_parallel_tests: usize) -> Self {
        match NonZeroUsize::new(max_parallel_tests) {
            Some(max_parallel_tests) if max_parallel_tests.get() > 1 => Self {
                max_parallel_tests: Some(max_parallel_tests),
            },
            _ => Self::sequential(),
        }
    }

    /// Read the number of tests to run at the same time from `BROWSER_TEST_PARALLELISM`.
    ///
    /// See [`Self::from_env_var`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a number.
    pub fn from_env() -> Result<Option<Self>, InvalidEnvVar> {
        Self::from_env_var(DEFAULT_PARALLELISM_ENV)
    }

    /// Read the number of tests to run at the same time from `env_var`.
    ///
    /// Returns `None` if the variable is unset or empty, so the caller picks the default:
    /// `Parallelism::from_env()?.unwrap_or(Parallelism::parallel(4))`. The value is a number as
    /// accepted by [`Self::parallel`]. The variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a number.
    pub fn from_env_var(env_var: impl AsRef<str>) -> Result<Option<Self>, InvalidEnvVar> {
        Ok(env_number(env_var.as_ref())?.map(Self::parallel))
    }

    pub(crate) const fn max_parallel_tests(self) -> NonZeroUsize {
        match self.max_parallel_tests {
            Some(max_parallel_tests) => max_parallel_tests,
            None => NonZeroUsize::MIN,
        }
    }
}

/// How [`crate::BrowserTestRunner`] handles failed browser tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailurePolicy {
    /// Stop starting tests after the first failed test.
    ///
    /// Tests in groups marked [`crate::BrowserTests::run_always`] still run. Tests that already
    /// started when the failure occurred run to completion, and their failures are reported too.
    /// If only one test can run at a time, its failure is returned as is. Otherwise, failures are
    /// returned as child reports of a [`crate::BrowserTestError::RunTests`] report.
    #[default]
    FailFast,

    /// Run every test and return all failures as child reports of one
    /// [`crate::BrowserTestError::RunTests`] report.
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

        Err(crate::failure_report::without_own_location(
            failure_collection.context(BrowserTestError::RunTests { failed_tests }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;
    use crate::test_support::EnvVarGuard;

    #[test]
    fn parallel_treats_zero_and_one_as_sequential() {
        assert_that!(Parallelism::default()).is_equal_to(Parallelism::sequential());
        assert_that!(Parallelism::parallel(0)).is_equal_to(Parallelism::sequential());
        assert_that!(Parallelism::parallel(1)).is_equal_to(Parallelism::sequential());
        assert_that!(Parallelism::sequential().max_parallel_tests().get()).is_equal_to(1);
        assert_that!(Parallelism::parallel(4).max_parallel_tests().get()).is_equal_to(4);
    }

    #[test]
    fn from_env_reads_the_number_of_parallel_tests() {
        let env = EnvVarGuard::new(DEFAULT_PARALLELISM_ENV);

        env.remove();
        assert_that!(Parallelism::from_env()).is_equal_to(Ok(None));

        env.set(" 4 ");
        assert_that!(Parallelism::from_env()).is_equal_to(Ok(Some(Parallelism::parallel(4))));

        env.set("many");
        assert_that!(Parallelism::from_env().is_err()).is_true();
    }

    #[test]
    fn from_env_var_reads_a_custom_variable() {
        let env = EnvVarGuard::new("BROWSER_TEST_CUSTOM_PARALLELISM");
        env.set("3");

        assert_that!(Parallelism::from_env_var("BROWSER_TEST_CUSTOM_PARALLELISM"))
            .is_equal_to(Ok(Some(Parallelism::parallel(3))));
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

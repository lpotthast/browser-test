//! How the tests of a run are scheduled: how many run at the same time, and whether a failure
//! stops the run.

use std::num::NonZeroUsize;

use crate::env::{InvalidEnvVar, env_positive_number};

/// Default environment variable read by [`Parallelism::from_env`].
pub(crate) const DEFAULT_PARALLELISM_ENV: &str = "BROWSER_TEST_PARALLELISM";

/// How many entries of a [`crate::TestSuite`] run at the same time, each test in its own
/// session.
///
/// Sequential by default. Tests take parallel slots in their given order. A test whose browser is
/// already running may start before an earlier test whose browser is still starting. Only run
/// tests in parallel that can use the same application state at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Parallelism {
    max_parallel_tests: NonZeroUsize,
}

impl Default for Parallelism {
    fn default() -> Self {
        Self::sequential()
    }
}

impl Parallelism {
    /// Run one test at a time.
    #[must_use]
    pub const fn sequential() -> Self {
        Self {
            max_parallel_tests: NonZeroUsize::MIN,
        }
    }

    /// Run up to `max_parallel_tests` tests at the same time. `1` runs tests sequentially.
    ///
    /// # Panics
    ///
    /// Panics if `max_parallel_tests` is zero, as no test could run.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use browser_test::{TestSuite, Parallelism};
    ///
    /// let tests = TestSuite::<()>::parallel(Parallelism::parallel(4));
    /// ```
    #[must_use]
    #[track_caller]
    pub const fn parallel(max_parallel_tests: usize) -> Self {
        match NonZeroUsize::new(max_parallel_tests) {
            Some(max_parallel_tests) => Self { max_parallel_tests },
            None => panic!("a parallelism of zero would run no test"),
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
    /// `Parallelism::from_env()?.unwrap_or(Parallelism::parallel(4))`. The value is a positive
    /// number, `1` running tests sequentially. The variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a positive number.
    pub fn from_env_var(env_var: impl AsRef<str>) -> Result<Option<Self>, InvalidEnvVar> {
        Ok(env_positive_number(env_var.as_ref())?
            .map(|max_parallel_tests| Self { max_parallel_tests }))
    }

    /// The most tests that run at the same time, `1` if sequential.
    #[must_use]
    pub const fn max_parallel_tests(self) -> NonZeroUsize {
        self.max_parallel_tests
    }
}

/// How [`crate::BrowserTestRunner`] handles failed browser tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum FailurePolicy {
    /// Stop starting tests after the first failed test.
    ///
    /// Tests in suites marked [`crate::TestSuite::run_always`] still run. Tests that already
    /// started when the failure occurred run to completion, and their failures are reported too.
    /// A test still waiting for its browser session doesn't start. The failures are returned as
    /// child reports of one [`crate::BrowserTestError::RunTests`] report.
    #[default]
    FailFast,

    /// Run every test and return all failures as child reports of one
    /// [`crate::BrowserTestError::RunTests`] report.
    RunAll,
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;
    use crate::test_support::EnvVarGuard;

    #[test]
    fn parallel_treats_one_as_sequential() {
        assert_that!(Parallelism::default()).is_equal_to(Parallelism::sequential());
        assert_that!(Parallelism::parallel(1)).is_equal_to(Parallelism::sequential());
        assert_that!(Parallelism::sequential().max_parallel_tests().get()).is_equal_to(1);
        assert_that!(Parallelism::parallel(4).max_parallel_tests().get()).is_equal_to(4);
    }

    #[test]
    #[should_panic(expected = "would run no test")]
    fn parallel_rejects_zero() {
        let _ = Parallelism::parallel(0);
    }

    #[test]
    fn from_env_reads_the_number_of_parallel_tests() {
        let env = EnvVarGuard::new(DEFAULT_PARALLELISM_ENV);

        env.remove();
        assert_that!(Parallelism::from_env()).is_equal_to(Ok(None));

        env.set(" 4 ");
        assert_that!(Parallelism::from_env()).is_equal_to(Ok(Some(Parallelism::parallel(4))));

        env.set("many");
        assert_that!(Parallelism::from_env()).is_err();

        env.set("0");
        assert_that!(Parallelism::from_env()).is_err();
    }

    #[test]
    fn from_env_var_reads_a_custom_variable() {
        let env = EnvVarGuard::new("BROWSER_TEST_CUSTOM_PARALLELISM");
        env.set("3");

        assert_that!(Parallelism::from_env_var("BROWSER_TEST_CUSTOM_PARALLELISM"))
            .is_equal_to(Ok(Some(Parallelism::parallel(3))));
    }
}

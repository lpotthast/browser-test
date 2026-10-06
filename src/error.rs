/// Error contexts reported by browser-test runner operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BrowserTestError {
    /// Chrome for Testing could not be resolved, downloaded, or launched.
    #[error("Failed to launch Chrome for Testing.")]
    LaunchChromeForTesting,

    /// A browser test failed while running in its `WebDriver` session.
    #[error("Browser test '{test_name}' failed.")]
    RunTest {
        /// The test name reported by [`crate::BrowserTest::name`].
        test_name: String,
    },

    /// A browser test panicked while running in its `WebDriver` session.
    #[error("Browser test '{test_name}' panicked: {message}")]
    Panic {
        /// The test name reported by [`crate::BrowserTest::name`].
        test_name: String,
        /// A string representation of the panic payload.
        message: String,
    },

    /// One or more browser tests failed or panicked. Each failure is a child report.
    ///
    /// Returned for every failed run, except [`crate::FailurePolicy::FailFast`] runs in which only
    /// one test can run at a time. These return the failed test's report directly.
    #[error("One or more browser tests failed or panicked ({failed_tests} failed).")]
    RunTests {
        /// Number of failed or panicked tests collected as child reports.
        failed_tests: usize,
    },

    /// Chrome for Testing could not be shut down cleanly.
    #[error("Failed to shut down Chrome for Testing.")]
    ShutDownChromeForTesting,

    /// The pause message or prompt could not be written to stdout.
    #[error("Failed to flush pause prompt.")]
    FlushPausePrompt,

    /// The pause response could not be read from stdin.
    #[error("Failed to read pause response from stdin.")]
    ReadPauseResponse,
}

#[cfg(test)]
mod tests {
    use super::*;
    use assertr::prelude::*;

    #[test]
    fn run_test_error_displays_plain_test_name() {
        let err = BrowserTestError::RunTest {
            test_name: "login".to_owned(),
        };

        assert_that!(err.to_string()).is_equal_to("Browser test 'login' failed.");
    }

    #[test]
    fn run_tests_error_displays_failure_count() {
        let err = BrowserTestError::RunTests { failed_tests: 2 };

        assert_that!(err.to_string())
            .is_equal_to("One or more browser tests failed or panicked (2 failed).");
    }

    #[test]
    fn panic_error_displays_test_name_and_message() {
        let err = BrowserTestError::Panic {
            test_name: "login".to_owned(),
            message: "assertion failed".to_owned(),
        };

        assert_that!(err.to_string())
            .is_equal_to("Browser test 'login' panicked: assertion failed");
    }
}

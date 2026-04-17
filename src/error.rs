/// Error contexts reported by browser-test runner operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrowserTestError {
    /// Chromedriver could not be started.
    #[error("Failed to start webdriver.")]
    StartWebdriver,

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

    /// Multiple browser tests failed or panicked and were collected into one report.
    #[error("One or more browser tests failed or panicked ({failed_tests} failed).")]
    RunTests {
        /// Number of failed or panicked tests collected as child reports.
        failed_tests: usize,
    },

    /// Chromedriver could not be terminated cleanly.
    #[error("Failed to terminate chromedriver.")]
    TerminateWebdriver,

    /// The pause prompt could not be flushed to stdout.
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

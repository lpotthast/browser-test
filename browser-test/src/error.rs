use std::time::Duration;

use crate::report::FormatDuration;

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

    /// The `WebDriver` session of a browser test could not be prepared: it could not be created,
    /// ended before the test started, or did not accept the test's timeouts. The test did not run.
    #[error("The browser session of browser test '{test_name}' could not be prepared.")]
    PrepareSession {
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

    /// A browser test ran longer than its time limit (see
    /// [`BrowserTestRunner::with_time_limit`](crate::BrowserTestRunner::with_time_limit) and
    /// [`TestSettings::with_time_limit`](crate::TestSettings::with_time_limit)).
    #[error("Browser test '{test_name}' exceeded its time limit of {}.", FormatDuration(*limit))]
    TimedOut {
        /// The test name reported by [`crate::BrowserTest::name`].
        test_name: String,
        /// The time limit it exceeded.
        limit: Duration,
    },

    /// One or more browser tests failed, panicked, or timed out. Each failure is a child report, in
    /// test order. Returned for every run in which a test failed.
    #[error("One or more browser tests failed, panicked or timed out ({failed_tests} failed).")]
    RunTests {
        /// Number of failed, panicked, or timed out tests collected as child reports.
        failed_tests: usize,
    },

    /// The directory holding the run's Chrome profiles could not be created.
    #[error("Failed to create the directory for Chrome profiles.")]
    CreateChromeProfiles,

    /// The run's cancellation token was cancelled, so the run stopped early. Errors of the run, if
    /// any, are child reports.
    ///
    /// See [`crate::Cancellation`].
    #[error("The browser test run was cancelled.")]
    Cancelled,

    /// The shutdown signals requested with [`crate::Cancellation::on_shutdown_signals`] could not
    /// be listened for.
    #[error("Failed to listen for shutdown signals.")]
    ListenForShutdownSignals,

    /// Chrome for Testing could not be shut down cleanly.
    #[error("Failed to shut down Chrome for Testing.")]
    ShutDownChromeForTesting,

    /// The pause message or prompt could not be written to stdout.
    #[error("Failed to write the pause prompt.")]
    WritePausePrompt,

    /// The pause response could not be read from stdin.
    #[error("Failed to read pause response from stdin.")]
    ReadPauseResponse,
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

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
            .is_equal_to("One or more browser tests failed, panicked or timed out (2 failed).");
    }

    #[test]
    fn timed_out_error_displays_test_name_and_limit() {
        let err = BrowserTestError::TimedOut {
            test_name: "login".to_owned(),
            limit: Duration::from_secs(30),
        };

        assert_that!(err.to_string())
            .is_equal_to("Browser test 'login' exceeded its time limit of 30.00s.");
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

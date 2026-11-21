//! The failures of a run's tests, and the run's result.

use rootcause::{Report, report_collection::ReportCollection};

use crate::BrowserTestError;

/// The failures of the tests of a run, with the index of each failed test.
#[derive(Default)]
pub(super) struct BrowserTestFailures {
    failures: Vec<(usize, Report<BrowserTestError>)>,
}

impl BrowserTestFailures {
    pub(super) fn push(&mut self, test_index: usize, failure: Report<BrowserTestError>) {
        self.failures.push((test_index, failure));
    }

    /// The result of the run: one [`BrowserTestError::RunTests`] report holding every failure, in
    /// test order, if a test failed.
    pub(super) fn into_result(mut self) -> Result<(), Report<BrowserTestError>> {
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

    #[test]
    fn browser_test_failures_returns_ok_when_empty() {
        let failures = BrowserTestFailures::default();

        assert_that!(failures.into_result()).is_ok();
    }

    #[test]
    fn browser_test_failures_returns_aggregate_report_with_children() {
        let mut failures = BrowserTestFailures::default();
        failures.push(
            1,
            Report::new(BrowserTestError::RunTest {
                test_name: "checkout".to_owned(),
            }),
        );
        failures.push(
            0,
            Report::new(BrowserTestError::RunTest {
                test_name: "login".to_owned(),
            }),
        );

        let err = failures
            .into_result()
            .expect_err("non-empty failure collection should fail");

        assert_that!(err.to_string())
            .contains(BrowserTestError::RunTests { failed_tests: 2 }.to_string());
        assert_that!(err).has_child_count(2);
        // In test order.
        let report = format!("{err:?}");
        assert_that!(report.find("login") < report.find("checkout")).is_true();
    }
}

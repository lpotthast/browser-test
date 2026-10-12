use browser_test::browser_test;
use browser_test::thirtyfour::WebDriver;
use rootcause::Report;

use crate::Context;
use crate::tests::{WikipediaTestError, goto, wait_for_visible};

/// The search input is visible.
#[browser_test]
pub(crate) async fn search_input_is_visible(
    driver: &WebDriver,
    context: &Context,
) -> Result<(), Report<WikipediaTestError>> {
    goto(driver, context.base_url).await?;
    wait_for_visible(driver, "input[name='search']").await
}

use browser_test::browser_test;
use browser_test::thirtyfour::WebDriver;
use rootcause::Report;

use crate::Context;
use crate::tests::{WikipediaTestError, goto, title_contains};

/// The page title names Wikipedia.
#[browser_test]
pub(crate) async fn title_contains_wikipedia(
    driver: &WebDriver,
    context: &Context,
) -> Result<(), Report<WikipediaTestError>> {
    goto(driver, context.base_url).await?;
    title_contains(driver, "Wikipedia").await
}

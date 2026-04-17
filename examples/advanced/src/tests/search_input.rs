use std::borrow::Cow;

use browser_test::thirtyfour::WebDriver;
use browser_test::{BrowserTest, async_trait};
use rootcause::Report;

use crate::Context;
use crate::tests::{WikipediaTestError, goto, wait_for_visible};

pub(crate) struct SearchInputIsVisible;

#[async_trait]
impl BrowserTest<Context, WikipediaTestError> for SearchInputIsVisible {
    fn name(&self) -> Cow<'_, str> {
        "search_input_is_visible".into()
    }

    async fn run(
        &self,
        driver: &WebDriver,
        context: &Context,
    ) -> Result<(), Report<WikipediaTestError>> {
        goto(driver, context.base_url).await?;
        wait_for_visible(driver, "input[name='search']").await?;
        Ok(())
    }
}

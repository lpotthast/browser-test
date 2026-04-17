use std::borrow::Cow;

use browser_test::thirtyfour::WebDriver;
use browser_test::{BrowserTest, async_trait};
use rootcause::Report;

use crate::Context;
use crate::tests::{WikipediaTestError, goto, title_contains};

pub(crate) struct TitleContainsWikipedia;

#[async_trait]
impl BrowserTest<Context, WikipediaTestError> for TitleContainsWikipedia {
    fn name(&self) -> Cow<'_, str> {
        "title_contains_wikipedia".into()
    }

    async fn run(
        &self,
        driver: &WebDriver,
        context: &Context,
    ) -> Result<(), Report<WikipediaTestError>> {
        goto(driver, context.base_url).await?;
        title_contains(driver, "Wikipedia").await
    }
}

use std::borrow::Cow;

use browser_test::thirtyfour::WebDriver;
use browser_test::{BrowserTest, async_trait};
use rootcause::Report;

use crate::Context;
use crate::tests::{goto, wait_for_visible};

/// The link to the Wikipedia edition in `language` is visible.
///
/// Implements `BrowserTest` itself, as `#[browser_test]` functions take no parameters of their own.
pub(crate) struct EditionIsLinked {
    pub(crate) language: &'static str,
}

#[async_trait]
impl BrowserTest<Context> for EditionIsLinked {
    fn name(&self) -> Cow<'_, str> {
        format!("{}::edition_is_linked::{}", module_path!(), self.language).into()
    }

    fn description(&self) -> Option<Cow<'_, str>> {
        Some(
            format!(
                "The link to the {:?} Wikipedia edition is visible.",
                self.language
            )
            .into(),
        )
    }

    async fn run(&self, driver: &WebDriver, context: &Context) -> Result<(), Report> {
        goto(driver, context.base_url).await?;
        wait_for_visible(driver, &format!("#js-link-box-{}", self.language)).await?;
        Ok(())
    }
}

use browser_test::thirtyfour::prelude::ElementQueryable;
use browser_test::thirtyfour::{By, WebDriver};
use rootcause::Report;
use rootcause::prelude::ResultExt;

mod search_input;
mod title;

pub(crate) use search_input::SearchInputIsVisible;
pub(crate) use title::TitleContainsWikipedia;

#[derive(Debug, thiserror::Error)]
pub(crate) enum WikipediaTestError {
    #[error("failed to open the page")]
    OpenPage,

    #[error("failed to read the page title")]
    ReadTitle,

    #[error("unexpected page title: expected it to contain {expected:?}, got {actual:?}")]
    UnexpectedTitleContains {
        expected: &'static str,
        actual: String,
    },

    #[error("failed to find visible element {selector:?}")]
    FindVisibleElement { selector: String },
}

#[tracing::instrument(
    name = "browser_test_step",
    skip_all,
    fields(helper = "goto", url = %url),
)]
async fn goto(driver: &WebDriver, url: &str) -> Result<(), Report<WikipediaTestError>> {
    driver
        .goto(url)
        .await
        .context(WikipediaTestError::OpenPage)?;
    Ok(())
}

#[tracing::instrument(
    name = "browser_test_step",
    skip_all,
    fields(helper = "title_contains", expected = %expected),
)]
async fn title_contains(
    driver: &WebDriver,
    expected: &'static str,
) -> Result<(), Report<WikipediaTestError>> {
    let title = driver
        .title()
        .await
        .context(WikipediaTestError::ReadTitle)?;
    if !title.contains(expected) {
        return Err(Report::new(WikipediaTestError::UnexpectedTitleContains {
            expected,
            actual: title,
        }));
    }

    Ok(())
}

#[tracing::instrument(
    name = "browser_test_step",
    skip_all,
    fields(helper = "wait_for_visible", selector = %selector),
)]
async fn wait_for_visible(
    driver: &WebDriver,
    selector: &str,
) -> Result<(), Report<WikipediaTestError>> {
    driver
        .query(By::Css(selector))
        .and_displayed()
        .first()
        .await
        .context(WikipediaTestError::FindVisibleElement {
            selector: selector.to_owned(),
        })?;
    Ok(())
}

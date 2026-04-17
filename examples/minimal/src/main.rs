use browser_test::thirtyfour::WebDriver;
use browser_test::{
    BrowserTest, BrowserTestError, BrowserTestRunner, BrowserTestVisibility, BrowserTests,
    async_trait,
};
use rootcause::{Report, report};
use std::borrow::Cow;

const BASE_URL: &str = "https://www.wikipedia.org/";

struct PageTitleTest;

#[async_trait]
impl BrowserTest for PageTitleTest {
    fn name(&self) -> Cow<'_, str> {
        "page title".into()
    }

    async fn run(&self, driver: &WebDriver, _context: &()) -> Result<(), Report> {
        driver.goto(BASE_URL).await?;

        let title = driver.title().await?;
        if !title.contains("Wikipedia") {
            return Err(report!(
                "unexpected page title: expected it to contain \"Wikipedia\", got {title:?}",
            ));
        }
        Ok(())
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Report<BrowserTestError>> {
    tracing_subscriber::fmt::init();

    BrowserTestRunner::new()
        .with_visibility(BrowserTestVisibility::Visible)
        .run(&(), BrowserTests::new().with(PageTitleTest))
        .await
}

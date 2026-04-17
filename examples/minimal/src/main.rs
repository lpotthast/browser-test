use std::borrow::Cow;

use browser_test::thirtyfour::WebDriver;
use browser_test::{
    BrowserTest, BrowserTestError, BrowserTestRunner, BrowserTestVisibility, BrowserTests,
    async_trait,
};
use rootcause::{Report, report};

struct Context {
    base_url: String,
}

struct PageTitleTest;

#[async_trait]
impl BrowserTest<Context> for PageTitleTest {
    fn name(&self) -> Cow<'_, str> {
        "page title".into()
    }

    async fn run(&self, driver: &WebDriver, context: &Context) -> Result<(), Report> {
        driver.goto(&context.base_url).await?;

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

    let context = Context {
        base_url: "https://www.wikipedia.org".into(),
    };

    BrowserTestRunner::new()
        .with_visibility(BrowserTestVisibility::Visible)
        .run(&context, BrowserTests::new().with(PageTitleTest))
        .await
}

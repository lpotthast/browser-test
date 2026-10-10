use browser_test::thirtyfour::WebDriver;
use browser_test::{
    BrowserTestError, BrowserTestRunner, BrowserTests, Cancellation, Visibility, browser_test,
};
use rootcause::{Report, report};

struct Context {
    base_url: String,
}

/// The page title names Wikipedia.
#[browser_test]
async fn page_title(driver: &WebDriver, context: &Context) -> Result<(), Report> {
    driver.goto(&context.base_url).await?;

    let title = driver.title().await?;
    if !title.contains("Wikipedia") {
        return Err(report!(
            "unexpected page title: expected it to contain \"Wikipedia\", got {title:?}",
        ));
    }
    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Report<BrowserTestError>> {
    tracing_subscriber::fmt::init();

    let context = Context {
        base_url: "https://www.wikipedia.org".into(),
    };

    BrowserTestRunner::new(Cancellation::on_shutdown_signals())
        .with_visibility(Visibility::Visible)
        .run(&context, BrowserTests::sequential().with(PageTitle))
        .await
}

//! A function not named in `PascalCase` is rejected, with the name to use. Its struct is still
//! defined, so that registering the test reports no error of its own.

use browser_test::{TestSuite, browser_test};
use rootcause::Report;

#[browser_test]
async fn page_title() -> Result<(), Report> {
    Ok(())
}

#[browser_test]
async fn __() -> Result<(), Report> {
    Ok(())
}

fn main() {
    let _: TestSuite<()> = TestSuite::sequential().with(PageTitle);
}

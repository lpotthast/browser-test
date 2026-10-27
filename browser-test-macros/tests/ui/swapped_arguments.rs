use browser_test::{browser_test, thirtyfour::WebDriver};
use rootcause::Report;

#[browser_test]
async fn swapped(_context: &str, _driver: &WebDriver) -> Result<(), Report> {
    Ok(())
}

fn main() {}

use browser_test::{browser_test, thirtyfour::WebDriver};

#[browser_test]
async fn three(_driver: &WebDriver, _context: &(), _extra: &()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

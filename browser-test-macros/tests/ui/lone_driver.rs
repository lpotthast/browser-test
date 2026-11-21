use browser_test::browser_test;

#[browser_test]
async fn DriverOnly(_driver: &browser_test::thirtyfour::WebDriver) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

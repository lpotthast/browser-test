use browser_test::browser_test;

#[browser_test]
async fn driver_only(_driver: &browser_test::thirtyfour::WebDriver) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

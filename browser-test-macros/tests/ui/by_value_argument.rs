use browser_test::browser_test;

#[browser_test]
async fn by_value(_context: String) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

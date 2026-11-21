use browser_test::browser_test;

#[browser_test]
async fn ByValue(_context: String) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

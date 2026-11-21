use browser_test::browser_test;

#[browser_test]
async fn Result() -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

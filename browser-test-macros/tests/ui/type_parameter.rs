use browser_test::browser_test;

#[browser_test]
async fn generic<T: Sync>(_context: &T) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

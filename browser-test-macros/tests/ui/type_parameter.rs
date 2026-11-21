use browser_test::browser_test;

#[browser_test]
async fn Generic<T: Sync>(_context: &T) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

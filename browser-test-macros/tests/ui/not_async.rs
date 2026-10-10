use browser_test::browser_test;

#[browser_test]
fn synchronous(_context: &()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

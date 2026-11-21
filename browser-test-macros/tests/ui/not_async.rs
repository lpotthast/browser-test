use browser_test::browser_test;

#[browser_test]
fn Synchronous(_context: &()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

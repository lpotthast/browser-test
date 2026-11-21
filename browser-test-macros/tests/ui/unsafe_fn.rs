use browser_test::browser_test;

#[browser_test]
async unsafe fn UnsafeTest(_context: &()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

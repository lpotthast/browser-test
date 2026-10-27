use browser_test::browser_test;

#[browser_test]
async unsafe fn unsafe_test(_context: &()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

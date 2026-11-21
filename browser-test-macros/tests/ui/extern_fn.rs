use browser_test::browser_test;

#[browser_test]
async extern "C" fn ExternTest(_context: &()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

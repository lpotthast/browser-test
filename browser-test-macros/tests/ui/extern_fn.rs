use browser_test::browser_test;

#[browser_test]
async extern "C" fn extern_test(_context: &()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

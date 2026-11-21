use browser_test::browser_test;

#[browser_test]
async fn MutableContext(_context: &mut ()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

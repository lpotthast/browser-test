use browser_test::browser_test;

#[browser_test]
async fn mutable_context(_context: &mut ()) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

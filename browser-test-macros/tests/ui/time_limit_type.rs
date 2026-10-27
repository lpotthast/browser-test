use browser_test::browser_test;

#[browser_test(time_limit = 5)]
async fn seconds() -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

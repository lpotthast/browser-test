use browser_test::browser_test;
use rootcause::Report;

#[browser_test(crate = not_browser_test)]
async fn UnknownCrate() -> Result<(), Report> {
    Ok(())
}

fn main() {}

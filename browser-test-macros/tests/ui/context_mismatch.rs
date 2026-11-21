use browser_test::{TestSuite, browser_test};
use rootcause::Report;

#[browser_test]
async fn TakesANumber(_context: &u32) -> Result<(), Report> {
    Ok(())
}

fn main() {
    let _: TestSuite<str> = TestSuite::sequential().with(TakesANumber);
}

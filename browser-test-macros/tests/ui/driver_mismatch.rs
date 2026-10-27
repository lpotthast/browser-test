use browser_test::{TestSuite, browser_test};
use rootcause::Report;

#[browser_test]
async fn takes_a_string(_driver: &String, _context: &str) -> Result<(), Report> {
    Ok(())
}

fn main() {
    let _: TestSuite<str> = TestSuite::sequential().with(TakesAString);
}

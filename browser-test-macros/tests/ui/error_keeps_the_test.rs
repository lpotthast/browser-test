//! An error of the attribute keeps the function, whose body is still type-checked, and its struct,
//! which registering the test still finds.

use browser_test::{TestSuite, browser_test};
use rootcause::Report;

#[browser_test(time_limt = std::time::Duration::from_secs(5))]
async fn misspelled_argument(_context: &str) -> Result<(), Report> {
    let _: u32 = "not a number";
    Ok(())
}

#[browser_test]
pub fn not_async(_context: &str) -> Result<(), Report> {
    Ok(())
}

fn main() {
    let _: TestSuite<str> = TestSuite::sequential()
        .with(MisspelledArgument)
        .with(NotAsync);
}

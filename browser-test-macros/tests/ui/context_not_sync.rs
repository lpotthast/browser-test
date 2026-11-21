use std::cell::Cell;

use browser_test::browser_test;

#[browser_test]
async fn Counts(_context: &Cell<u32>) -> Result<(), rootcause::Report> {
    Ok(())
}

fn main() {}

#![deny(dead_code)]

use browser_test::browser_test;
use rootcause::Report;

#[browser_test]
async fn forgotten() -> Result<(), Report> {
    Ok(())
}

fn main() {}

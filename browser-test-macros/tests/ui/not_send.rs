use std::rc::Rc;

use browser_test::browser_test;
use rootcause::Report;

#[browser_test]
async fn HoldsRcAcrossAwait() -> Result<(), Report> {
    let shared = Rc::new(());
    std::future::ready(()).await;
    drop(shared);
    Ok(())
}

fn main() {}

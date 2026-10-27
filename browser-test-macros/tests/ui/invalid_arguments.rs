use browser_test::browser_test;
use rootcause::Report;

#[browser_test(path = "/checkout")]
async fn unknown_key() -> Result<(), Report> {
    Ok(())
}

#[browser_test(name = "one", name = "two")]
async fn duplicate_key() -> Result<(), Report> {
    Ok(())
}

#[browser_test(name = "")]
async fn empty_name() -> Result<(), Report> {
    Ok(())
}

#[browser_test(tags = "smoke")]
async fn unbracketed_tags() -> Result<(), Report> {
    Ok(())
}

#[browser_test(tags = ["smoke", ""])]
async fn empty_tag() -> Result<(), Report> {
    Ok(())
}

#[browser_test(tags = ["smoke", "keyboard", "smoke"])]
async fn duplicate_tag() -> Result<(), Report> {
    Ok(())
}

fn main() {}

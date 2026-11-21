use browser_test::browser_test;
use rootcause::Report;

#[browser_test(path = "/checkout")]
async fn UnknownKey() -> Result<(), Report> {
    Ok(())
}

#[browser_test(name = "one", name = "two")]
async fn DuplicateKey() -> Result<(), Report> {
    Ok(())
}

#[browser_test(name = "")]
async fn EmptyName() -> Result<(), Report> {
    Ok(())
}

#[browser_test(tags = "smoke")]
async fn UnbracketedTags() -> Result<(), Report> {
    Ok(())
}

#[browser_test(tags = ["smoke", ""])]
async fn EmptyTag() -> Result<(), Report> {
    Ok(())
}

#[browser_test(tags = ["smoke", "keyboard", "smoke"])]
async fn DuplicateTag() -> Result<(), Report> {
    Ok(())
}

fn main() {}

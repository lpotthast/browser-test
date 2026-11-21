use browser_test::browser_test;

struct Tests;

impl Tests {
    #[browser_test]
    async fn method(&self) -> Result<(), rootcause::Report> {
        Ok(())
    }
}

fn main() {}

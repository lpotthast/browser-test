//! The attribute's public API, including contexts borrowed for only one test run.

use assertr::prelude::*;
use browser_test::{BrowserTest, TestFilter, TestSuite, browser_test};
use rootcause::Report;

struct Page<'a> {
    title: &'a str,
}

/// Documentation and visibility belong to the generated test type.
///
/// The page title matches the expected title.
///
///     Indented Markdown stays indented.
#[browser_test]
pub(crate) async fn borrowed_page(page: &Page<'_>) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_that!(page.title).is_equal_to("title");
    Ok(())
}

#[browser_test]
#[allow(clippy::elidable_lifetime_names)] // Exercise explicit lifetime syntax.
async fn explicit_lifetime<'page>(page: &Page<'page>) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_that!(page.title).is_equal_to("title");
    Ok(())
}

#[browser_test]
async fn nested_reference(context: &&str) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_that!(*context).is_equal_to("title");
    Ok(())
}

#[browser_test]
async fn raw_driver(
    _driver: &browser_test::thirtyfour::WebDriver,
    _context: &str,
) -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

#[browser_test(tags = ["smoke"])]
async fn no_context() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

/// Keyboard events reach their handler.
#[browser_test(name = "keyboard::events", tags = ["smoke", "keyboard"])]
async fn handles_keyboard_events() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

#[browser_test]
#[doc = concat!("Computed", " documentation.")]
async fn computed_documentation() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

// Attributes other than docs and `cfg` stay on the function: neither is valid on a struct.
#[browser_test]
#[inline]
#[expect(unused_variables)]
async fn function_attributes() -> Result<(), Report> {
    let unused = ();
    tokio::task::yield_now().await;
    Ok(())
}

#[browser_test]
async fn r#type() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("custom error")]
struct CustomError;

type Outcome = Result<(), Report<CustomError>>;

#[browser_test]
async fn typed_error(_context: &()) -> Outcome {
    tokio::task::yield_now().await;
    Err(Report::new(CustomError))
}

// A disabled test must not leave behind an impl referring to the removed struct.
#[browser_test]
#[cfg(any())]
async fn disabled(_context: &DoesNotExist) -> Result<(), Report> {
    unreachable!()
}

struct Callbacks<'a> {
    on_title: fn(&str) -> usize,
    on_click: &'a (dyn Fn(&str) -> usize + Sync),
}

// Lifetimes elided in `fn` pointers and `Fn` bounds stay higher-ranked.
#[browser_test]
async fn higher_ranked(callbacks: &Callbacks<'_>) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_that!((callbacks.on_title)("title")).is_equal_to(5);
    assert_that!((callbacks.on_click)("button")).is_equal_to(6);
    Ok(())
}

/// Macros of the names the generated code uses don't change the test's name.
mod shadowed_macros {
    use super::*;

    #[allow(unused_macros)]
    macro_rules! concat {
        ($($tt:tt)*) => {
            "shadowed"
        };
    }

    #[allow(unused_macros)]
    macro_rules! module_path {
        () => {
            "shadowed"
        };
    }

    #[browser_test]
    pub(super) async fn named_by_its_path() -> Result<(), Report> {
        tokio::task::yield_now().await;
        Ok(())
    }
}

mod other {
    use super::*;

    #[browser_test(tags = ["smoke"])]
    pub(super) async fn borrowed_page(_context: &()) -> Result<(), Report> {
        tokio::task::yield_now().await;
        Ok(())
    }
}

#[test]
fn generated_tests_implement_the_trait_with_distinct_module_qualified_names() {
    fn accepts_borrowed(_test: impl for<'a> BrowserTest<Page<'a>>) {}
    fn accepts_nested(_test: impl for<'a> BrowserTest<&'a str>) {}
    fn accepts_higher_ranked(_test: impl for<'a> BrowserTest<Callbacks<'a>>) {}
    accepts_borrowed(BorrowedPage);
    accepts_borrowed(ExplicitLifetime);
    accepts_nested(NestedReference);
    accepts_higher_ranked(HigherRanked);
    assert_that!(BorrowedPage.name()).is_equal_to("macros::borrowed_page");
    assert_that!(Type.name()).is_equal_to("macros::type");
    assert_that!(other::BorrowedPage.name()).is_equal_to("macros::other::borrowed_page");
    assert_that!(shadowed_macros::NamedByItsPath.name())
        .is_equal_to("macros::shadowed_macros::named_by_its_path");
    let _: TestSuite<str> = TestSuite::sequential().with(RawDriver);
    // Tests of different error types share a suite.
    let _: TestSuite = TestSuite::sequential()
        .with(TypedError)
        .with(FunctionAttributes);
}

#[test]
fn tags_select_generated_tests() {
    assert_that!(HandlesKeyboardEvents.tags()).contains_exactly(["smoke", "keyboard"]);
    assert_that!(NoContext.tags()).contains_exactly(["smoke"]);
    assert_that!(FunctionAttributes.tags()).is_empty();
    let tests = TestSuite::sequential()
        .with(HandlesKeyboardEvents)
        .with(NoContext)
        .with(other::BorrowedPage)
        .with(FunctionAttributes)
        .filter(
            &TestFilter::new()
                .with_tag("smoke")
                .with_name("other::borrowed_page")
                .with_name("no_context"),
        );
    assert_that!(format!("{tests:?}"))
        .contains(r#"entries: ["macros::no_context", "macros::other::borrowed_page"]"#);
}

#[test]
fn names_and_descriptions_are_independent() {
    let test = HandlesKeyboardEvents;
    assert_that!(test.name()).is_equal_to("keyboard::events");
    assert_that!(test.description().as_deref())
        .is_equal_to(Some("Keyboard events reach their handler."));
    assert_that!(
        TestSuite::sequential()
            .with(test)
            .filter(&TestFilter::new().with_name("keyboard::events"))
            .is_empty()
    )
    .is_false();
    assert_that!(
        TestSuite::sequential()
            .with(HandlesKeyboardEvents)
            .filter(&TestFilter::new().with_name("Keyboard events reach"))
            .is_empty()
    )
    .is_true();
    assert_that!(BorrowedPage.description().as_deref()).is_equal_to(Some(concat!(
        "Documentation and visibility belong to the generated test type.\n\n",
        "The page title matches the expected title.\n\n",
        "    Indented Markdown stays indented.",
    )));
    assert_that!(ComputedDocumentation.description().as_deref())
        .is_equal_to(Some("Computed documentation."));
    assert_that!(NoContext.description()).is_none();
}

#[tokio::test]
async fn functions_keep_their_name_async_results_and_borrowed_arguments() {
    let title = String::from("title");
    let page = Page { title: &title };
    assert_that!(borrowed_page(&page).await).is_ok();
    assert_that!(explicit_lifetime(&page).await).is_ok();
    assert_that!(typed_error(&()).await).is_err();
    let callbacks = Callbacks {
        on_title: str::len,
        on_click: &str::len,
    };
    assert_that!(higher_ranked(&callbacks).await).is_ok();
}

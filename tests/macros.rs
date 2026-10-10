//! The attribute's public API, including contexts borrowed for only one test run.

use browser_test::{BrowserTest, BrowserTests, TestFilter, TestGroup, browser_test};
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
    assert_eq!(page.title, "title");
    Ok(())
}

#[browser_test]
#[allow(clippy::elidable_lifetime_names)] // Exercise explicit lifetime syntax.
async fn explicit_lifetime<'page>(page: &Page<'page>) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_eq!(page.title, "title");
    Ok(())
}

#[browser_test]
async fn nested_reference(context: &&str) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_eq!(*context, "title");
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

#[browser_test]
async fn no_context() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

/// Keyboard events reach their handler.
#[browser_test(name = "keyboard::events")]
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
    assert_eq!((callbacks.on_title)("title"), 5);
    assert_eq!((callbacks.on_click)("button"), 6);
    Ok(())
}

mod other {
    use super::*;

    #[browser_test]
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
    assert_eq!(BorrowedPage.name(), "macros::borrowed_page");
    assert_eq!(Type.name(), "macros::type");
    assert_eq!(other::BorrowedPage.name(), "macros::other::borrowed_page");
    let _: BrowserTests = BrowserTests::sequential().with(FunctionAttributes);
    let _: BrowserTests<str> = BrowserTests::sequential().with(RawDriver);
    let _: BrowserTests<(), CustomError> = BrowserTests::sequential().with(TypedError);
    let tests = BrowserTests::sequential()
        .with_test_group(
            TestGroup::new("smoke")
                .with(NoContext)
                .with(other::BorrowedPage),
        )
        .filter(
            &TestFilter::new()
                .with_group("smoke")
                .with_name_containing("other::borrowed_page"),
        );
    assert!(!tests.is_empty());
    assert!(
        tests
            .filter(&TestFilter::new().with_name_containing("no_context"))
            .is_empty()
    );
}

#[test]
fn names_and_descriptions_are_independent_and_survive_wrapping() {
    let test = HandlesKeyboardEvents;
    assert_eq!(test.name(), "keyboard::events");
    assert_eq!(
        test.description().as_deref(),
        Some("Keyboard events reach their handler.")
    );
    assert!(
        !BrowserTests::sequential()
            .with(test)
            .filter(&TestFilter::new().with_name_containing("keyboard::events"))
            .is_empty()
    );
    assert!(
        BrowserTests::sequential()
            .with(HandlesKeyboardEvents)
            .filter(&TestFilter::new().with_name_containing("Keyboard events reach"))
            .is_empty()
    );
    assert_eq!(
        HandlesKeyboardEvents
            .named("renamed")
            .description()
            .as_deref(),
        Some("Keyboard events reach their handler.")
    );
    assert_eq!(
        BorrowedPage.description().as_deref(),
        Some(concat!(
            "Documentation and visibility belong to the generated test type.\n\n",
            "The page title matches the expected title.\n\n",
            "    Indented Markdown stays indented.",
        ))
    );
    assert_eq!(
        ComputedDocumentation.description().as_deref(),
        Some("Computed documentation.")
    );
    assert_eq!(NoContext.description(), None);
}

#[tokio::test]
async fn functions_keep_their_name_async_results_and_borrowed_arguments() {
    let title = String::from("title");
    let page = Page { title: &title };
    borrowed_page(&page).await.unwrap();
    explicit_lifetime(&page).await.unwrap();
    assert!(typed_error(&()).await.is_err());
    let callbacks = Callbacks {
        on_title: str::len,
        on_click: &str::len,
    };
    higher_ranked(&callbacks).await.unwrap();
}

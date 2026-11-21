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
pub(crate) async fn BorrowedPage(page: &Page<'_>) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_that!(page.title).is_equal_to("title");
    Ok(())
}

#[browser_test]
#[allow(clippy::elidable_lifetime_names)] // Exercise explicit lifetime syntax.
async fn ExplicitLifetime<'page>(page: &Page<'page>) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_that!(page.title).is_equal_to("title");
    Ok(())
}

#[browser_test]
async fn NestedReference(context: &&str) -> Result<(), Report> {
    tokio::task::yield_now().await;
    assert_that!(*context).is_equal_to("title");
    Ok(())
}

#[browser_test]
async fn RawDriver(
    _driver: &browser_test::thirtyfour::WebDriver,
    _context: &str,
) -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

#[browser_test(tags = ["smoke"])]
async fn NoContext() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

/// Keyboard events reach their handler.
#[browser_test(name = "keyboard::events", tags = ["smoke", "keyboard"])]
async fn HandlesKeyboardEvents() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

#[browser_test]
#[doc = concat!("Computed", " documentation.")]
async fn ComputedDocumentation() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

// Attributes other than docs and `cfg` stay on the function: neither is valid on a struct.
#[browser_test]
#[inline]
#[expect(unused_variables)]
async fn FunctionAttributes() -> Result<(), Report> {
    let unused = ();
    tokio::task::yield_now().await;
    Ok(())
}

/// Lint attributes for the struct reach it: its `#[allow]`s, and the `missing_docs` and `dead_code` of
/// its `#[expect]`s.
#[deny(missing_docs, unfulfilled_lint_expectations)]
pub mod lints {
    use super::*;

    #[browser_test]
    #[allow(missing_docs)]
    pub async fn Allowed() -> Result<(), Report> {
        tokio::task::yield_now().await;
        Ok(())
    }

    #[browser_test]
    #[expect(
        missing_docs,
        unused_variables,
        reason = "the attribute splits its lints"
    )]
    pub async fn Expected() -> Result<(), Report> {
        let unused = ();
        tokio::task::yield_now().await;
        Ok(())
    }

    // Only the struct is unused when a test is never registered.
    #[browser_test]
    #[expect(dead_code)]
    async fn Unregistered() -> Result<(), Report> {
        tokio::task::yield_now().await;
        Ok(())
    }

    // `#[cfg_attr]`s are expanded before `#[browser_test]` sees them.
    #[browser_test]
    #[cfg_attr(all(), doc = "Documented conditionally.")]
    pub async fn ConditionallyDocumented() -> Result<(), Report> {
        tokio::task::yield_now().await;
        Ok(())
    }
}

#[browser_test]
#[cfg_attr(all(), cfg(any()))]
async fn DisabledConditionally(_context: &DoesNotExist) -> Result<(), Report> {
    unreachable!()
}

#[browser_test]
async fn Loop() -> Result<(), Report> {
    tokio::task::yield_now().await;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("custom error")]
struct CustomError;

type Outcome = Result<(), Report<CustomError>>;

#[browser_test]
async fn TypedError(_context: &()) -> Outcome {
    tokio::task::yield_now().await;
    Err(Report::new(CustomError))
}

// A disabled test must not leave behind an impl referring to the removed struct.
#[browser_test]
#[cfg(any())]
async fn Disabled(_context: &DoesNotExist) -> Result<(), Report> {
    unreachable!()
}

struct Callbacks<'a> {
    on_title: fn(&str) -> usize,
    on_click: &'a (dyn Fn(&str) -> usize + Sync),
}

// Lifetimes elided in `fn` pointers and `Fn` bounds stay higher-ranked.
#[browser_test]
async fn HigherRanked(callbacks: &Callbacks<'_>) -> Result<(), Report> {
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
    pub(super) async fn NamedByItsPath() -> Result<(), Report> {
        tokio::task::yield_now().await;
        Ok(())
    }
}

/// `crate = ...` replaces the path to browser-test, e.g. in a crate re-exporting it.
mod reexported {
    use super::*;

    mod reexports {
        pub(super) use ::browser_test as reexported;
    }

    #[browser_test(crate = self::reexports::reexported)]
    pub(super) async fn CratePath() -> Result<(), Report> {
        tokio::task::yield_now().await;
        Ok(())
    }
}

mod other {
    use super::*;

    #[browser_test(tags = ["smoke"])]
    pub(super) async fn BorrowedPage(_context: &()) -> Result<(), Report> {
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
    assert_that!(BorrowedPage.name()).is_equal_to("macros::BorrowedPage");
    // A test without arguments implements `BrowserTest` for every context, which calls name.
    assert_that!(BrowserTest::<()>::name(&Loop)).is_equal_to("macros::Loop");
    assert_that!(other::BorrowedPage.name()).is_equal_to("macros::other::BorrowedPage");
    assert_that!(BrowserTest::<()>::name(&reexported::CratePath))
        .is_equal_to("macros::reexported::CratePath");
    assert_that!(BrowserTest::<()>::name(&shadowed_macros::NamedByItsPath))
        .is_equal_to("macros::shadowed_macros::NamedByItsPath");
    let _: TestSuite<str> = TestSuite::sequential().with(RawDriver);
    // Tests without arguments join suites of every context.
    let _: TestSuite<str> = TestSuite::sequential().with(NoContext).with(RawDriver);
    let _: TestSuite<Page<'_>> = TestSuite::sequential().with(NoContext).with(BorrowedPage);
    // Tests of different error types share a suite.
    let _: TestSuite = TestSuite::sequential()
        .with(TypedError)
        .with(FunctionAttributes);
}

#[test]
fn tags_select_generated_tests() {
    assert_that!(BrowserTest::<()>::tags(&HandlesKeyboardEvents))
        .contains_exactly(["smoke", "keyboard"]);
    assert_that!(BrowserTest::<()>::tags(&NoContext)).contains_exactly(["smoke"]);
    assert_that!(BrowserTest::<()>::tags(&FunctionAttributes)).is_empty();
    let tests = TestSuite::sequential()
        .with(HandlesKeyboardEvents)
        .with(NoContext)
        .with(other::BorrowedPage)
        .with(FunctionAttributes)
        .filter(
            &TestFilter::new()
                .with_tag("smoke")
                .with_name("other::BorrowedPage")
                .with_name("NoContext"),
        );
    assert_that!(format!("{tests:?}"))
        .contains(r#"entries: ["macros::NoContext", "macros::other::BorrowedPage"]"#);
}

#[test]
fn names_and_descriptions_are_independent() {
    let test: &dyn BrowserTest = &HandlesKeyboardEvents;
    assert_that!(test.name()).is_equal_to("keyboard::events");
    assert_that!(test.description().as_deref())
        .is_equal_to(Some("Keyboard events reach their handler."));
    assert_that!(
        TestSuite::<()>::sequential()
            .with(HandlesKeyboardEvents)
            .filter(&TestFilter::new().with_name("keyboard::events"))
            .is_empty()
    )
    .is_false();
    assert_that!(
        TestSuite::<()>::sequential()
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
    assert_that!(BrowserTest::<()>::description(&ComputedDocumentation).as_deref())
        .is_equal_to(Some("Computed documentation."));
    assert_that!(BrowserTest::<()>::description(&NoContext)).is_none();
    assert_that!(BrowserTest::<()>::description(&lints::ConditionallyDocumented).as_deref())
        .is_equal_to(Some("Documented conditionally."));
}

#[tokio::test]
async fn functions_are_named_in_snake_case_and_keep_async_results_and_borrowed_arguments() {
    let title = String::from("title");
    let page = Page { title: &title };
    assert_that!(borrowed_page(&page).await).is_ok();
    assert_that!(explicit_lifetime(&page).await).is_ok();
    assert_that!(typed_error(&()).await).is_err();
    // Keywords are raw identifiers.
    assert_that!(r#loop().await).is_ok();
    let callbacks = Callbacks {
        on_title: str::len,
        on_click: &str::len,
    };
    assert_that!(higher_ranked(&callbacks).await).is_ok();
}

//! The `#[browser_test]` attribute of [browser-test](https://docs.rs/browser-test).
//!
//! Depend on `browser-test` and use the attribute through its re-export,
//! `browser_test::browser_test`. The generated code refers to `browser-test`, under whatever name
//! your `Cargo.toml` gives it.

mod browser_test;

use proc_macro2::TokenStream;
use syn::ItemFn;

/// Turn an async function into a browser test: a unit struct implementing `BrowserTest`.
///
/// ```
/// use browser_test::{BrowserTest, BrowserTests, browser_test, thirtyfour::WebDriver};
/// use rootcause::Report;
///
/// /// The home page greets the visitor.
/// #[browser_test]
/// async fn shows_greeting(base_url: &str) -> Result<(), Report> {
///     // Navigate and assert through a page object or helpers taking `base_url`.
///     Ok(())
/// }
///
/// /// Opens the page itself, through the session's `WebDriver`.
/// #[browser_test(name = "navigation::home")]
/// async fn opens_home_page(driver: &WebDriver, base_url: &str) -> Result<(), Report> {
///     driver.goto(base_url).await?;
///     Ok(())
/// }
///
/// assert!(ShowsGreeting.name().ends_with("::shows_greeting"));
/// assert_eq!(ShowsGreeting.description().as_deref(), Some("The home page greets the visitor."));
/// assert_eq!(OpensHomePage.name(), "navigation::home");
///
/// let tests: BrowserTests<str> = BrowserTests::sequential()
///     .with(ShowsGreeting)
///     .with(OpensHomePage);
/// ```
///
/// # The generated test
///
/// `async fn shows_greeting` becomes `struct ShowsGreeting;`, a unit struct named in `PascalCase`
/// with the function's visibility, its doc comments and its `#[cfg]`s. It derives `Debug`,
/// `Clone`, `Copy` and `Default`. Register it with `BrowserTests::with(ShowsGreeting)`. The
/// function itself becomes the test's body: it is no longer callable under its own name, and all
/// its other attributes (e.g. `#[allow]`, `#[expect]`, `#[tracing::instrument]`) stay on it.
///
/// - **Name**: `BrowserTest::name` returns the module-qualified function name
///   (`module_path!()` and the function name, e.g. `my_tests::checkout::shows_greeting`), which
///   `TestFilter` matches. `#[browser_test(name = "checkout::greeting")]` sets another one, e.g.
///   one that stays the same when the function moves to another module.
/// - **Description**: `BrowserTest::description` returns the doc comments, as written, without the
///   space after each `///`. Paragraphs and indentation are kept. `None` without doc comments.
/// - **Session settings**: the runner's. Implement `BrowserTest` yourself, with
///   `BrowserTest::session_settings`, for tests that need their own timeouts, element query wait,
///   or a fresh session.
///
/// # Signature
///
/// The function must be an `async fn` returning `Result<(), Report>` or
/// `Result<(), Report<E>>` (also through a type alias), with its future `Send`. It takes
///
/// - no arguments,
/// - the run's context: `&Context`, implementing `BrowserTest<Context>`, or
/// - the session's driver and the run's context: `&WebDriver, &Context`.
///
/// The context can be unsized (`&str`) or borrow from the run (`&Page<'_>`): the generated test
/// then implements `BrowserTest<Page<'a>>` for every lifetime `'a`. Lifetime parameters are
/// allowed; type and const parameters, `self` and `&mut` arguments are not. A test without
/// arguments implements `BrowserTest<()>`.
///
/// Functions named `none`, `some`, `ok` or `err` are rejected: their unit structs would shadow
/// `None`, `Some`, `Ok` and `Err` in the function's module.
#[manyhow::manyhow]
#[proc_macro_attribute]
pub fn browser_test(
    args: browser_test::Arguments,
    function: ItemFn,
) -> manyhow::Result<TokenStream> {
    browser_test::expand(args, function)
}

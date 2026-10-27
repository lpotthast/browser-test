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
/// use std::time::Duration;
///
/// use browser_test::{BrowserTest, TestSuite, browser_test, thirtyfour::WebDriver};
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
/// #[browser_test(name = "navigation::home", tags = ["smoke"], time_limit = Duration::from_secs(10))]
/// async fn opens_home_page(driver: &WebDriver, base_url: &str) -> Result<(), Report> {
///     driver.goto(base_url).await?;
///     Ok(())
/// }
///
/// assert!(ShowsGreeting.name().ends_with("::shows_greeting"));
/// assert_eq!(ShowsGreeting.description().as_deref(), Some("The home page greets the visitor."));
/// assert_eq!(OpensHomePage.name(), "navigation::home");
/// assert_eq!(OpensHomePage.tags(), ["smoke"]);
/// assert_eq!(OpensHomePage.settings().time_limit(), Some(Duration::from_secs(10)));
/// assert_eq!(ShowsGreeting.settings().time_limit(), None);
///
/// let tests: TestSuite<str> = TestSuite::sequential()
///     .with(ShowsGreeting)
///     .with(OpensHomePage);
/// ```
///
/// # The generated test
///
/// `async fn shows_greeting` becomes `struct ShowsGreeting;`, a unit struct named in `PascalCase`
/// with the function's visibility, its doc comments and its `#[cfg]`s. It derives `Debug`,
/// `Clone`, `Copy` and `Default`. Register it with `TestSuite::with(ShowsGreeting)`. The
/// function itself stays as the test's body, private to its module, under its own name (which
/// failure reports and `#[tracing::instrument]` show), and all its other attributes (e.g.
/// `#[allow]`, `#[expect]`, `#[tracing::instrument]`) stay on it.
///
/// - **Name**: `BrowserTest::name` returns the module-qualified function name
///   (`module_path!()` and the function name, e.g. `my_tests::checkout::shows_greeting`), which
///   `TestFilter` matches. `#[browser_test(name = "checkout::greeting")]` sets another one, e.g.
///   one that stays the same when the function moves to another module.
/// - **Description**: `BrowserTest::description` returns the doc comments, as written, without the
///   space after each `///`. Paragraphs and indentation are kept. `None` without doc comments.
/// - **Tags**: `BrowserTest::tags` returns the tags `tags = ["...", ...]` lists, if given, which
///   `TestFilter::with_tag` selects. No tags otherwise.
/// - **Settings**: `BrowserTest::settings` takes everything from the runner, except the time limit
///   `time_limit = ...` sets, if given (any expression of type `std::time::Duration`). Implement
///   `BrowserTest` yourself, with `BrowserTest::settings`, for tests that need their own timeouts,
///   element query wait, or a fresh session.
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
/// `BrowserTest::run` returns a `Report<E>` as a `Report`, keeping `E` as its context.
///
/// The context can be unsized (`&str`) or borrow from the run (`&Page<'_>`): the generated test
/// then implements `BrowserTest<Page<'a>>` for every lifetime `'a`. Lifetime parameters are
/// allowed. Type and const parameters, `self` and `&mut` arguments are not. A test without
/// arguments implements `BrowserTest<()>`.
///
/// A single `&WebDriver` argument is rejected, as a single argument is the context. Take
/// `(driver: &WebDriver, context: &Context)`, with `&()` for a run without context.
///
/// A test never registered with `TestSuite` warns as an unused function (`dead_code`), unless
/// its struct is public.
///
/// Functions whose struct would shadow a name of the standard prelude in the function's module are
/// rejected, e.g. `result` (`Result`), `copy` (`Copy`) or `ok` (`Ok`). Rename them, and keep the
/// test's name with `name = "..."`.
#[manyhow::manyhow]
#[proc_macro_attribute]
pub fn browser_test(
    args: browser_test::Arguments,
    function: ItemFn,
) -> manyhow::Result<TokenStream> {
    browser_test::expand(args, function)
}

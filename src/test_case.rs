use std::{borrow::Cow, fmt};

use async_trait::async_trait;
use rootcause::Report;
use thirtyfour::WebDriver;

use crate::{BrowserTimeouts, ElementQueryWaitConfig};

/// A browser test that can run against one fresh `WebDriver` session.
#[async_trait]
pub trait BrowserTest<C = (), E = rootcause::markers::Dynamic>: Send + Sync
where
    C: Sync + ?Sized,
    E: ?Sized,
{
    /// A human-readable test name for logs and failure context.
    ///
    /// The runner owns the returned name before running the test body, so implementations may
    /// return either a borrowed name stored on the test or a freshly generated owned name.
    fn name(&self) -> Cow<'_, str>;

    /// Optional timeouts for this test.
    ///
    /// Returning `None` uses the runner's default timeout configuration, if one is set.
    fn timeouts(&self) -> Option<BrowserTimeouts> {
        None
    }

    /// Optional element query wait configuration for this test.
    ///
    /// Returning `None` uses the runner's default element query wait configuration, if one is set.
    fn element_query_wait(&self) -> Option<ElementQueryWaitConfig> {
        None
    }

    /// Execute the test body.
    async fn run(&self, driver: &WebDriver, context: &C) -> Result<(), Report<E>>;
}

/// A collection of browser tests for [`crate::BrowserTestRunner`].
///
/// This type erases each concrete test into the boxed trait object used by the runner while
/// keeping call sites concise.
///
/// # Examples
///
/// ```rust,no_run
/// # use std::borrow::Cow;
/// # use browser_test::thirtyfour::WebDriver;
/// # use browser_test::{async_trait, BrowserTest, BrowserTests};
/// # use rootcause::Report;
/// # struct OpensHomePage;
///
/// #[async_trait]
/// impl BrowserTest for OpensHomePage {
///     fn name(&self) -> Cow<'_, str> { "opens home page".into() }
///     async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> { Ok(()) }
/// }
///
/// struct SearchWorks;
/// #[async_trait]
/// impl BrowserTest for SearchWorks {
///     fn name(&self) -> Cow<'_, str> { "search works".into() }
///     async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> { Ok(()) }
/// }
///
/// let tests = BrowserTests::new()
///     .with(OpensHomePage)
///     .with(SearchWorks);
/// ```
pub struct BrowserTests<C = (), E = rootcause::markers::Dynamic>
where
    C: Sync + ?Sized,
    E: ?Sized,
{
    tests: Vec<Box<dyn BrowserTest<C, E>>>,
}

impl<C, E> BrowserTests<C, E>
where
    C: Sync + ?Sized,
    E: ?Sized,
{
    /// Creates an empty browser test collection.
    #[must_use]
    pub const fn new() -> Self {
        Self { tests: Vec::new() }
    }

    /// Adds a test and returns the collection for chaining.
    #[must_use]
    pub fn with<T>(mut self, test: T) -> Self
    where
        T: BrowserTest<C, E> + 'static,
    {
        self.push(test);
        self
    }

    /// Adds a test to the collection.
    pub fn push<T>(&mut self, test: T) -> &mut Self
    where
        T: BrowserTest<C, E> + 'static,
    {
        self.tests.push(Box::new(test));
        self
    }

    /// Returns `true` if the collection contains no tests.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tests.is_empty()
    }

    pub(crate) fn into_vec(self) -> Vec<Box<dyn BrowserTest<C, E>>> {
        self.tests
    }
}

impl<C, E> Default for BrowserTests<C, E>
where
    C: Sync + ?Sized,
    E: ?Sized,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<C, E> fmt::Debug for BrowserTests<C, E>
where
    C: Sync + ?Sized,
    E: ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserTests")
            .field("tests", &BrowserTestNames(&self.tests))
            .finish()
    }
}

struct BrowserTestNames<'a, C, E>(&'a [Box<dyn BrowserTest<C, E>>])
where
    C: Sync + ?Sized,
    E: ?Sized;

impl<C, E> fmt::Debug for BrowserTestNames<'_, C, E>
where
    C: Sync + ?Sized,
    E: ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|test| test.name()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NamedTest(&'static str);

    #[async_trait::async_trait]
    impl BrowserTest for NamedTest {
        fn name(&self) -> Cow<'_, str> {
            Cow::Borrowed(self.0)
        }

        async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
            Ok(())
        }
    }

    #[test]
    fn browser_tests_debug_prints_test_names() {
        let tests = BrowserTests::new()
            .with(NamedTest("opens home page"))
            .with(NamedTest("search works"));

        assert_eq!(
            format!("{tests:?}"),
            r#"BrowserTests { tests: ["opens home page", "search works"] }"#
        );
    }
}

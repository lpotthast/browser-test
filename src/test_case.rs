use std::{borrow::Cow, fmt};

use async_trait::async_trait;
use rootcause::Report;
use thirtyfour::WebDriver;

use crate::{ElementQueryWait, Parallelism, Timeouts};

/// A browser test that can run against one fresh `WebDriver` session.
// `async_trait` marks the boxed futures `#[must_use]`, which they are already.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BrowserTest<Context = (), TestError = rootcause::markers::Dynamic>: Send + Sync
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    /// A human-readable test name for logs and failure context.
    ///
    /// The runner owns the returned name before running the test body, so implementations may
    /// return either a borrowed name stored on the test or a freshly generated owned name.
    fn name(&self) -> Cow<'_, str>;

    /// Optional timeouts for this test.
    ///
    /// Returning `None` uses the runner's default timeout configuration, if one is set.
    fn timeouts(&self) -> Option<Timeouts> {
        None
    }

    /// Optional element query wait configuration for this test.
    ///
    /// Returning `None` uses the runner's default element query wait configuration, if one is set.
    fn element_query_wait(&self) -> Option<ElementQueryWait> {
        None
    }

    /// Execute the test body.
    async fn run(&self, driver: &WebDriver, context: &Context) -> Result<(), Report<TestError>>;
}

/// A group of browser tests and nested groups, and how they run.
///
/// A group runs its entries either one after another ([`Self::sequential`]) or up to a number of
/// entries at the same time ([`Self::parallel`]). Entries are tests ([`Self::with`]) and nested
/// groups ([`Self::with_group`]), so any mix of sequential and parallel execution can be expressed:
/// a sequential group of groups runs stages one after another, and a sequential group inside a
/// parallel group keeps its tests from running at the same time while other tests run alongside.
///
/// Entries start in the order they were added. Every test runs in a fresh browser session. How
/// failures affect the run is decided by [`crate::BrowserTestRunner::with_failure_policy`]: with
/// [`crate::FailurePolicy::RunAll`] every test runs; with [`crate::FailurePolicy::FailFast`] no
/// further test starts after a failure, except in groups marked [`Self::run_always`].
///
/// # Examples
///
/// ```rust,no_run
/// # use std::borrow::Cow;
/// # use browser_test::thirtyfour::WebDriver;
/// # use browser_test::{async_trait, BrowserTest, BrowserTests, Parallelism};
/// # use rootcause::Report;
/// # macro_rules! test {
/// #     ($name:ident) => {
/// #         struct $name;
/// #         #[async_trait]
/// #         impl BrowserTest for $name {
/// #             fn name(&self) -> Cow<'_, str> { stringify!($name).into() }
/// #             async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> { Ok(()) }
/// #         }
/// #     };
/// # }
/// # test!(Buttons); test!(Tables); test!(CreateUser); test!(DeleteUser); test!(ServerDidNotPanic);
/// let tests = BrowserTests::sequential()
///     .with_group(
///         BrowserTests::parallel(Parallelism::from_env()?.unwrap_or(Parallelism::parallel(4)))
///             .with(Buttons)
///             .with(Tables)
///             // These two share server state, so they must not run at the same time.
///             .with_group(BrowserTests::sequential().with(CreateUser).with(DeleteUser)),
///     )
///     .with_group(
///         BrowserTests::sequential()
///             .named("after all")
///             .run_always()
///             .with(ServerDidNotPanic),
///     );
/// # Ok::<(), browser_test::InvalidEnvVar>(())
/// ```
pub struct BrowserTests<Context = (), TestError = rootcause::markers::Dynamic>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    parallelism: Parallelism,
    name: Option<String>,
    run_always: bool,
    entries: Vec<BrowserTestEntry<Context, TestError>>,
}

/// An entry of a [`BrowserTests`] group.
pub(crate) enum BrowserTestEntry<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    Test(Box<dyn BrowserTest<Context, TestError>>),
    Group(BrowserTests<Context, TestError>),
}

impl<Context, TestError> BrowserTests<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    /// A group running its entries one after another.
    #[must_use]
    pub const fn sequential() -> Self {
        Self::parallel(Parallelism::sequential())
    }

    /// A group running up to `parallelism` of its entries at the same time.
    #[must_use]
    pub const fn parallel(parallelism: Parallelism) -> Self {
        Self {
            parallelism,
            name: None,
            run_always: false,
            entries: Vec::new(),
        }
    }

    /// Name this group. The run report lists the wall time of named groups, and the records of
    /// their tests carry the name.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Run this group's tests even after a failure stopped the run under
    /// [`crate::FailurePolicy::FailFast`], e.g. for checks that must see the whole run.
    #[must_use]
    pub const fn run_always(mut self) -> Self {
        self.run_always = true;
        self
    }

    /// Add a test.
    #[must_use]
    pub fn with<T>(mut self, test: T) -> Self
    where
        T: BrowserTest<Context, TestError> + 'static,
    {
        self.entries.push(BrowserTestEntry::Test(Box::new(test)));
        self
    }

    /// Add a nested group.
    #[must_use]
    pub fn with_group(mut self, group: Self) -> Self {
        self.entries.push(BrowserTestEntry::Group(group));
        self
    }

    /// Whether this group contains no test, directly or in nested groups.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|entry| match entry {
            BrowserTestEntry::Test(_) => false,
            BrowserTestEntry::Group(group) => group.is_empty(),
        })
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Parallelism,
        Option<String>,
        bool,
        Vec<BrowserTestEntry<Context, TestError>>,
    ) {
        (self.parallelism, self.name, self.run_always, self.entries)
    }
}

impl<Context, TestError> fmt::Debug for BrowserTests<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserTests")
            .field("parallelism", &self.parallelism)
            .field("name", &self.name)
            .field("run_always", &self.run_always)
            .field("entries", &self.entries)
            .finish()
    }
}

impl<Context, TestError> fmt::Debug for BrowserTestEntry<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Test(test) => write!(f, "{:?}", test.name()),
            Self::Group(group) => group.fmt(f),
        }
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
    fn browser_tests_debug_prints_the_tree_with_test_names() {
        let tests = BrowserTests::sequential()
            .with(NamedTest("opens home page"))
            .with_group(
                BrowserTests::sequential()
                    .named("nested")
                    .with(NamedTest("search works")),
            );

        assert_eq!(
            format!("{tests:?}"),
            concat!(
                r#"BrowserTests { parallelism: Parallelism { max_parallel_tests: None }, name: None, "#,
                r#"run_always: false, entries: ["opens home page", BrowserTests { parallelism: "#,
                r#"Parallelism { max_parallel_tests: None }, name: Some("nested"), run_always: false, "#,
                r#"entries: ["search works"] }] }"#,
            )
        );
    }

    #[test]
    fn groups_without_tests_are_empty() {
        let empty = BrowserTests::<()>::sequential().with_group(BrowserTests::sequential());
        assert!(empty.is_empty());
        assert!(!empty.with(NamedTest("test")).is_empty());
    }
}

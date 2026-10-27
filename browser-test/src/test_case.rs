use std::{borrow::Cow, fmt, panic::AssertUnwindSafe};

use async_trait::async_trait;
use rootcause::Report;
use thirtyfour::WebDriver;

use crate::{Parallelism, TestFilter, TestSettings};

/// A browser test: a named async body run in a `WebDriver` session, with the run's context.
///
/// Every test runs in a session of its own: a fresh one, or, with
/// [`SessionReuse`](crate::SessionReuse), one an earlier test ran in, reset. There are two ways
/// to define a test:
///
/// - [`#[browser_test]`](macro@crate::browser_test) on an `async fn`, for most tests: it generates a
///   unit struct implementing this trait, named after the function and documented by its doc
///   comments.
/// - Implement the trait, with [`async_trait`](crate::async_trait), for tests with settings of
///   their own ([`Self::settings`]), tests with parameters, and wrappers around other tests.
///
/// Either way, a test's metadata (name, description, tags, settings) is declared with the test.
///
/// `Context` is what [`BrowserTestRunner::run`](crate::BrowserTestRunner::run) gives every test,
/// e.g. the app's base URL. A failing test returns a [`Report`], to which the runner adds the
/// test's name. Helpers returning `Report<E>`s of an error type of their own convert into it with
/// `?`. Return an error of your own type with `rootcause::bail!(error)`.
///
/// # Examples
///
/// ```
/// use std::{borrow::Cow, time::Duration};
///
/// use browser_test::{BrowserTest, TestSettings, Timeouts, async_trait, thirtyfour::WebDriver};
/// use rootcause::Report;
///
/// /// Loads the first page with empty caches, so that its load time is that of a first visit.
/// struct FirstVisit {
///     path: &'static str,
/// }
///
/// #[async_trait]
/// impl BrowserTest<str> for FirstVisit {
///     fn name(&self) -> Cow<'_, str> {
///         format!("first visit of {}", self.path).into()
///     }
///
///     fn settings(&self) -> TestSettings {
///         TestSettings::new()
///             .with_timeouts(Timeouts::new().with_page_load(Duration::from_secs(5)))
///             .with_fresh_session(true)
///     }
///
///     async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
///         driver.goto(format!("{base_url}{}", self.path)).await?;
///         Ok(())
///     }
/// }
/// ```
///
/// # Wrapping tests
///
/// A test running another one, e.g. to check something after every test, must forward
/// [`Self::name`], [`Self::description`], [`Self::tags`] and [`Self::settings`] to it, or the
/// wrapped test's are lost:
///
/// ```
/// use std::borrow::Cow;
///
/// use browser_test::{BrowserTest, TestSettings, async_trait, thirtyfour::WebDriver};
/// use rootcause::Report;
///
/// /// Runs a test, then checks that its page logged no error.
/// struct CheckConsole<T>(T);
///
/// #[async_trait]
/// impl<T: BrowserTest<str>> BrowserTest<str> for CheckConsole<T> {
///     fn name(&self) -> Cow<'_, str> {
///         self.0.name()
///     }
///
///     fn description(&self) -> Option<Cow<'_, str>> {
///         self.0.description()
///     }
///
///     fn tags(&self) -> Vec<Cow<'_, str>> {
///         self.0.tags()
///     }
///
///     fn settings(&self) -> TestSettings {
///         self.0.settings()
///     }
///
///     async fn run(&self, driver: &WebDriver, base_url: &str) -> Result<(), Report> {
///         self.0.run(driver, base_url).await?;
///         // ... check the console ...
///         Ok(())
///     }
/// }
/// ```
// `async_trait` marks the boxed futures `#[must_use]`, which they are already.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BrowserTest<Context = ()>: Send + Sync
where
    Context: Sync + ?Sized,
{
    /// The test's name, used in logs, reports and failures, and matched by
    /// [`TestFilter`].
    ///
    /// Defaults to the Rust type name, e.g. `my_tests::checkout::ShowsTotal`. Names need not be
    /// unique, but distinct names keep reports unambiguous. The runner and filters read the name
    /// before the test runs: return the same name each time, borrowed or generated.
    fn name(&self) -> Cow<'_, str> {
        std::any::type_name::<Self>().into()
    }

    /// A description of what the test checks, if it has one.
    ///
    /// [`#[browser_test]`](macro@crate::browser_test) returns the function's doc comments. Unlike the
    /// name, the description is not matched by filters. Defaults to `None`.
    fn description(&self) -> Option<Cow<'_, str>> {
        None
    }

    /// Tags to select the test by, e.g. the component it checks, with [`TestFilter::with_tag`] and
    /// `BROWSER_TEST_TAG`.
    ///
    /// Tags don't change how the test runs, and are independent of its name and of the
    /// [`TestSuite`]s it runs in. `#[browser_test(tags = ["button"])]` sets them. Defaults to none.
    /// The attribute rejects empty and duplicate tags.
    ///
    /// The runner and filters read the tags before the test runs: return the same tags each time.
    fn tags(&self) -> Vec<Cow<'_, str>> {
        Vec::new()
    }

    /// What the test needs of the runner: timeouts, element query wait, a session no other test
    /// ran in, or a time limit. Defaults to [`TestSettings::new`], the runner's settings.
    /// `#[browser_test(time_limit = ...)]` sets a time limit.
    ///
    /// The runner reads the settings once, before the test runs.
    fn settings(&self) -> TestSettings {
        TestSettings::new()
    }

    /// Run the test in the session of `driver`, with the run's `context`.
    ///
    /// A returned error or a panic fails the test. Wrap the test's navigations, lookups and waits
    /// in [`Step`](crate::Step)s, and failure reports list the last of them.
    ///
    /// The runner polls the tests of a run on one task, as they borrow its context. Don't block
    /// the thread (`std::thread::sleep`, blocking I/O, heavy computation): that stalls every other
    /// test running at the same time. Await instead, or use `tokio::task::spawn_blocking`.
    async fn run(&self, driver: &WebDriver, context: &Context) -> Result<(), Report>;
}

/// A suite of browser tests and nested suites, and how they run.
///
/// A suite runs its entries either one after another ([`Self::sequential`]) or up to a number of
/// entries at the same time ([`Self::parallel`]). Entries are tests ([`Self::with`]) and nested
/// suites ([`Self::with_nested`]), so any mix of sequential and parallel execution can be expressed:
/// a sequential suite of suites runs stages one after another, and a sequential suite inside a
/// parallel suite keeps its tests from running at the same time while other tests run alongside.
/// To select tests independently of the suites they run in, tag them (see [`BrowserTest::tags`]).
///
/// Entries start in the order they were added. Every test runs in a fresh browser session, or in
/// the reset session of an earlier test (see [`crate::SessionReuse`]). How
/// failures affect the run is decided by [`crate::BrowserTestRunner::with_failure_policy`]. With
/// [`crate::FailurePolicy::RunAll`], every test runs. With [`crate::FailurePolicy::FailFast`], no
/// further test starts after a failure, except in suites marked [`Self::run_always`].
///
/// # Examples
///
/// ```rust,no_run
/// # use std::borrow::Cow;
/// # use browser_test::thirtyfour::WebDriver;
/// # use browser_test::{async_trait, BrowserTest, TestSuite, Parallelism};
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
/// let tests = TestSuite::sequential()
///     .with_nested(
///         TestSuite::parallel(Parallelism::from_env()?.unwrap_or(Parallelism::parallel(4)))
///             .with(Buttons)
///             .with(Tables)
///             // These two share server state, so they must not run at the same time.
///             .with_nested(TestSuite::sequential().with(CreateUser).with(DeleteUser)),
///     )
///     .with_nested(
///         TestSuite::sequential()
///             .named("after all")
///             .run_always()
///             .with(ServerDidNotPanic),
///     );
/// # Ok::<(), browser_test::InvalidEnvVar>(())
/// ```
pub struct TestSuite<Context = ()>
where
    Context: Sync + ?Sized,
{
    pub(crate) parallelism: Parallelism,
    pub(crate) name: Option<String>,
    pub(crate) run_always: bool,
    pub(crate) unfiltered: bool,
    pub(crate) entries: Vec<SuiteEntry<Context>>,
}

/// An entry of a [`TestSuite`].
pub(crate) enum SuiteEntry<Context>
where
    Context: Sync + ?Sized,
{
    Test(Box<dyn BrowserTest<Context>>),
    Suite(TestSuite<Context>),
}

impl<Context> TestSuite<Context>
where
    Context: Sync + ?Sized,
{
    /// A suite running its entries one after another.
    #[must_use]
    pub const fn sequential() -> Self {
        Self::parallel(Parallelism::sequential())
    }

    /// A suite running up to `parallelism` of its entries at the same time.
    #[must_use]
    pub const fn parallel(parallelism: Parallelism) -> Self {
        Self {
            parallelism,
            name: None,
            run_always: false,
            unfiltered: false,
            entries: Vec::new(),
        }
    }

    /// Name this suite. The run report lists the wall time of named suites, and the records of
    /// their tests carry the name.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Run this suite's tests even after a failure stopped the run under
    /// [`crate::FailurePolicy::FailFast`], e.g. for checks that must see the whole run.
    #[must_use]
    pub const fn run_always(mut self) -> Self {
        self.run_always = true;
        self
    }

    /// Keep all of this suite's tests, including those of nested suites, whatever the filter of
    /// the run (see [`crate::BrowserTestRunner::with_filter`]) or [`Self::filter`] selects, e.g. for
    /// checks of the whole run. Usually combined with [`Self::run_always`].
    #[must_use]
    pub const fn unfiltered(mut self) -> Self {
        self.unfiltered = true;
        self
    }

    /// Add a test.
    #[must_use]
    pub fn with<T>(mut self, test: T) -> Self
    where
        T: BrowserTest<Context> + 'static,
    {
        self.entries.push(SuiteEntry::Test(Box::new(test)));
        self
    }

    /// Add a nested suite as one entry of this suite. The nested suite runs its own entries
    /// sequentially or in parallel, as it was created.
    #[must_use]
    pub fn with_nested(mut self, suite: Self) -> Self {
        self.entries.push(SuiteEntry::Suite(suite));
        self
    }

    /// Keep only the tests `filter` selects, in this suite and nested ones, e.g.
    /// [`TestFilter::from_env`].
    ///
    /// Prefer giving the filter to the runner with
    /// [`BrowserTestRunner::with_filter`](crate::BrowserTestRunner::with_filter), which applies it
    /// the same way. Use this to inspect or reuse a filtered suite.
    ///
    /// A test whose name or tags panic while the filter reads them is kept, so that the run
    /// reports it as failed. Suites marked [`Self::unfiltered`] keep all their tests, which
    /// [`Self::run_always`] alone doesn't. Nested suites keep how they run, and empty ones are
    /// removed.
    #[must_use]
    pub fn filter(mut self, filter: &TestFilter) -> Self {
        if !filter.selects_all() {
            self.retain(&mut |test| filter.selects(test));
        }
        self
    }

    /// Keep only the tests `include` accepts, in this suite and nested ones not marked
    /// [`Self::unfiltered`], and remove nested suites left empty.
    fn retain(&mut self, include: &mut impl FnMut(&dyn BrowserTest<Context>) -> bool) {
        if self.unfiltered {
            return;
        }
        self.entries.retain_mut(|entry| match entry {
            SuiteEntry::Test(test) => include(test.as_ref()),
            SuiteEntry::Suite(suite) => {
                suite.retain(include);
                !suite.is_empty()
            }
        });
    }

    /// Whether this suite contains a test outside of [`Self::unfiltered`] suites, directly or in
    /// nested suites.
    pub(crate) fn has_filterable_tests(&self) -> bool {
        !self.unfiltered
            && self.entries.iter().any(|entry| match entry {
                SuiteEntry::Test(_) => true,
                SuiteEntry::Suite(suite) => suite.has_filterable_tests(),
            })
    }

    /// Whether this suite contains no test, directly or in nested suites.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|entry| match entry {
            SuiteEntry::Test(_) => false,
            SuiteEntry::Suite(suite) => suite.is_empty(),
        })
    }
}

impl<Context> fmt::Debug for TestSuite<Context>
where
    Context: Sync + ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestSuite")
            .field("parallelism", &self.parallelism)
            .field("name", &self.name)
            .field("run_always", &self.run_always)
            .field("unfiltered", &self.unfiltered)
            .field("entries", &self.entries)
            .finish()
    }
}

impl<Context> fmt::Debug for SuiteEntry<Context>
where
    Context: Sync + ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Test(test) => match std::panic::catch_unwind(AssertUnwindSafe(|| test.name())) {
                Ok(name) => name.fmt(f),
                Err(_) => f.write_str("<name() panicked>"),
            },
            Self::Suite(suite) => suite.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;
    use crate::test_support::{Metadata, StubTest};

    #[test]
    fn filters_keep_tests_whose_metadata_panics_as_they_read_it() {
        let suite = || {
            TestSuite::sequential()
                .with(StubTest::new("menu"))
                .with(StubTest::new("unnamed").panicking_in(Metadata::Name))
                .with(StubTest::new("untagged").panicking_in(Metadata::Tags))
        };
        let entries = |tests: TestSuite| format!("{:?}", tests.entries);

        // Kept, so that the run reports them as failed. A filter by name doesn't read tags.
        assert_that!(entries(
            suite().filter(&TestFilter::new().with_name("checkout"))
        ))
        .is_equal_to("[<name() panicked>]");
        assert_that!(entries(
            suite().filter(&TestFilter::new().with_tag("checkout"))
        ))
        .is_equal_to(r#"["untagged"]"#);
    }

    #[test]
    fn test_suite_debug_prints_the_tree_with_test_names() {
        let tests = TestSuite::sequential()
            .with(StubTest::new("opens home page"))
            .with_nested(
                TestSuite::sequential()
                    .named("nested")
                    .with(StubTest::new("search works")),
            );

        assert_that!(format!("{tests:?}")).is_equal_to(concat!(
            r#"TestSuite { parallelism: Parallelism { max_parallel_tests: 1 }, name: None, "#,
            r#"run_always: false, unfiltered: false, entries: ["opens home page", TestSuite { "#,
            r#"parallelism: Parallelism { max_parallel_tests: 1 }, name: Some("nested"), "#,
            r#"run_always: false, unfiltered: false, entries: ["search works"] }] }"#,
        ));
    }

    #[test]
    fn suites_without_tests_are_empty() {
        let empty = TestSuite::<()>::sequential().with_nested(TestSuite::sequential());
        assert_that!(empty.is_empty()).is_true();
        assert_that!(empty.with(StubTest::new("test")).is_empty()).is_false();
    }

    fn test_names(tests: &TestSuite) -> Vec<String> {
        tests
            .entries
            .iter()
            .flat_map(|entry| match entry {
                SuiteEntry::Test(test) => vec![test.name().into_owned()],
                SuiteEntry::Suite(suite) => test_names(suite),
            })
            .collect()
    }

    fn suite() -> TestSuite {
        TestSuite::parallel(Parallelism::parallel(2))
            .with(StubTest::new("untagged hover"))
            .with(StubTest::new("atom::hover").with_tags(&["buttons"]))
            .with(StubTest::new("hook::press").with_tags(&["buttons"]))
            .with_nested(
                TestSuite::sequential()
                    .named("stage")
                    .run_always()
                    .with(StubTest::new("other::hover").with_tags(&["buttons-extra"]))
                    .with(StubTest::new("keyboard::focus").with_tags(&["focus", "keyboard"])),
            )
    }

    #[test]
    fn selection_combines_tag_and_name_patterns_in_registration_order() {
        assert_that!(test_names(&suite().filter(&TestFilter::new()))).contains_exactly([
            "untagged hover",
            "atom::hover",
            "hook::press",
            "other::hover",
            "keyboard::focus",
        ]);
        assert_that!(test_names(
            &suite().filter(&TestFilter::new().with_name("hover"))
        ))
        .contains_exactly(["untagged hover", "atom::hover", "other::hover"]);
        assert_that!(test_names(
            &suite().filter(&TestFilter::new().with_tag("^buttons$"))
        ))
        .contains_exactly(["atom::hover", "hook::press"]);
        // Unanchored patterns match anywhere in a tag.
        assert_that!(test_names(
            &suite().filter(&TestFilter::new().with_tag("buttons"))
        ))
        .contains_exactly(["atom::hover", "hook::press", "other::hover"]);
        assert_that!(test_names(
            &suite().filter(&TestFilter::new().with_tag("keyboard"))
        ))
        .contains_exactly(["keyboard::focus"]);
        assert_that!(test_names(
            &suite().filter(
                &TestFilter::new()
                    .with_tag("^buttons$")
                    .with_tag("focus")
                    .with_name("press")
                    .with_name("focus")
            )
        ))
        .contains_exactly(["hook::press", "keyboard::focus"]);
        assert_that!(
            suite()
                .filter(&TestFilter::new().with_tag("missing"))
                .is_empty()
        )
        .is_true();
        assert_that!(
            suite()
                .filter(&TestFilter::new().with_tag("^buttons$").with_name("absent"))
                .is_empty()
        )
        .is_true();
    }

    #[test]
    fn selection_preserves_execution_policy() {
        let tests = suite().filter(&TestFilter::new().with_tag("focus"));
        assert_that!(tests.parallelism).is_equal_to(Parallelism::parallel(2));
        let SuiteEntry::Suite(stage) = &tests.entries[0] else {
            panic!("expected stage")
        };
        assert_that!(stage.name.as_deref()).is_equal_to(Some("stage"));
        assert_that!(stage.parallelism).is_equal_to(Parallelism::sequential());
        assert_that!(stage.run_always).is_true();
    }

    #[test]
    fn unfiltered_suites_keep_all_their_tests() {
        let tests = || {
            suite().with_nested(
                TestSuite::sequential()
                    .named("after all")
                    .run_always()
                    .unfiltered()
                    .with(StubTest::new("server did not panic"))
                    .with_nested(TestSuite::sequential().with(StubTest::new("no console errors"))),
            )
        };
        assert_that!(test_names(
            &tests().filter(&TestFilter::new().with_tag("focus"))
        ))
        .contains_exactly([
            "keyboard::focus",
            "server did not panic",
            "no console errors",
        ]);
        assert_that!(test_names(
            &tests().filter(&TestFilter::new().with_name("missing"))
        ))
        .contains_exactly(["server did not panic", "no console errors"]);
        // Marking the outermost suite exempts every test.
        assert_that!(
            suite()
                .unfiltered()
                .filter(&TestFilter::new().with_name("missing"))
                .is_empty()
        )
        .is_false();
    }
}

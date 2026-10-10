use std::{borrow::Cow, fmt, future::Future, panic::AssertUnwindSafe};

use async_trait::async_trait;
use rootcause::Report;
use thirtyfour::WebDriver;

use crate::{Parallelism, SessionSettings, TestFilter};

/// A browser test: a named async body run in a `WebDriver` session, with the run's context.
///
/// Every test runs in a session of its own: a fresh one, or, with
/// [`SessionReuse`](crate::SessionReuse), one an earlier test ran in, reset. There are three ways
/// to define a test:
///
/// - [`#[browser_test]`](macro@crate::browser_test) on an `async fn`, for most tests: it generates a
///   unit struct implementing this trait, named after the function and documented by its doc
///   comments.
/// - An `async fn` taking `&Context` is a test as it is, named by its Rust path. Use
///   [`Self::named`] to name it.
/// - Implement the trait, with [`async_trait`](crate::async_trait), for tests with session
///   settings of their own ([`Self::session_settings`]), tests with parameters, and wrappers around
///   other tests.
///
/// `Context` is what [`BrowserTestRunner::run`](crate::BrowserTestRunner::run) gives every test,
/// e.g. the app's base URL. `TestError` is the error type of the [`Report`] a failing test
/// returns. The runner adds the test's name to it.
///
/// # Examples
///
/// ```
/// use std::{borrow::Cow, time::Duration};
///
/// use browser_test::{BrowserTest, SessionSettings, Timeouts, async_trait, thirtyfour::WebDriver};
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
///     fn session_settings(&self) -> SessionSettings {
///         SessionSettings::new()
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
/// [`Self::name`], [`Self::description`] and [`Self::session_settings`] to it, or the wrapped
/// test's are lost:
///
/// ```
/// use std::borrow::Cow;
///
/// use browser_test::{BrowserTest, SessionSettings, async_trait, thirtyfour::WebDriver};
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
///     fn session_settings(&self) -> SessionSettings {
///         self.0.session_settings()
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
pub trait BrowserTest<Context = (), TestError = rootcause::markers::Dynamic>: Send + Sync
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    /// The test's name, used in logs, reports and failures, and matched by
    /// [`TestFilter`].
    ///
    /// Defaults to the Rust type name, e.g. `my_tests::checkout::ShowsTotal`, or the path of an
    /// `async fn`. Names need not be unique, but distinct names keep reports unambiguous. The
    /// runner reads the name once, before the test runs: return a borrowed or a generated name.
    fn name(&self) -> Cow<'_, str> {
        std::any::type_name::<Self>().into()
    }

    /// What the test checks, in prose, if it says.
    ///
    /// [`#[browser_test]`](macro@crate::browser_test) returns the function's doc comments. Unlike the
    /// name, the description is not matched by filters. Defaults to `None`.
    fn description(&self) -> Option<Cow<'_, str>> {
        None
    }

    /// What the test needs of its session: timeouts, element query wait, or a session no other
    /// test ran in. Defaults to [`SessionSettings::new`], the runner's settings.
    ///
    /// The runner reads the settings once, before the test runs.
    fn session_settings(&self) -> SessionSettings {
        SessionSettings::new()
    }

    /// This test under the name `name`, with its behavior, description and session settings.
    ///
    /// ```
    /// use browser_test::{BrowserTest, BrowserTests};
    /// use rootcause::Report;
    ///
    /// async fn opens_menu(_base_url: &str) -> Result<(), Report> {
    ///     Ok(())
    /// }
    ///
    /// let test = opens_menu.named("menu::opens");
    /// assert_eq!(test.name(), "menu::opens");
    /// let tests: BrowserTests<str> = BrowserTests::sequential().with(test);
    /// ```
    #[must_use]
    fn named(self, name: impl Into<String>) -> NamedTest<Self>
    where
        Self: Sized,
    {
        NamedTest {
            name: name.into(),
            test: self,
        }
    }

    /// Run the test in the session of `driver`, with the run's `context`.
    ///
    /// A returned error or a panic fails the test. Wrap the test's navigations, lookups and waits
    /// in [`Step`](crate::Step)s, and failure reports list the last of them.
    ///
    /// The runner polls the tests of a run on one task, as they borrow its context. Don't block
    /// the thread (`std::thread::sleep`, blocking I/O, heavy computation): that stalls every other
    /// test running at the same time. Await instead, or use `tokio::task::spawn_blocking`.
    async fn run(&self, driver: &WebDriver, context: &Context) -> Result<(), Report<TestError>>;
}

// Express the Send bound for a function's future without requiring callers to box it.
// Using the complete argument type keeps borrowed contexts valid under higher-ranked bounds.
trait TestFn<Argument, TestError: ?Sized + 'static>: Fn(Argument) -> Self::Run {
    type Run: Future<Output = Result<(), Report<TestError>>> + Send;
}

impl<Argument, TestError: ?Sized + 'static, F, Run> TestFn<Argument, TestError> for F
where
    F: Fn(Argument) -> Run,
    Run: Future<Output = Result<(), Report<TestError>>> + Send,
{
    type Run = Run;
}

/// An `async fn` taking the run's context is a test, named by its path, e.g.
/// `my_tests::checkout::shows_total`.
///
/// [`BrowserTest::named`] names it independently of its module. Use
/// [`#[browser_test]`](macro@crate::browser_test) for tests that also take the `WebDriver`, or should be
/// described by their doc comments.
#[async_trait]
impl<Context, TestError, F> BrowserTest<Context, TestError> for F
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
    F: for<'a> TestFn<&'a Context, TestError> + Send + Sync,
{
    async fn run(&self, _driver: &WebDriver, context: &Context) -> Result<(), Report<TestError>> {
        self(context).await
    }
}

/// A test under another name, created by [`BrowserTest::named`].
#[derive(Debug, Clone)]
pub struct NamedTest<T> {
    name: String,
    test: T,
}

#[async_trait]
impl<Context, TestError, T> BrowserTest<Context, TestError> for NamedTest<T>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
    T: BrowserTest<Context, TestError>,
{
    fn name(&self) -> Cow<'_, str> {
        self.name.as_str().into()
    }

    fn description(&self) -> Option<Cow<'_, str>> {
        self.test.description()
    }

    fn session_settings(&self) -> SessionSettings {
        self.test.session_settings()
    }

    async fn run(&self, driver: &WebDriver, context: &Context) -> Result<(), Report<TestError>> {
        self.test.run(driver, context).await
    }
}

/// A group of browser tests and nested groups, and how they run.
///
/// A group runs its entries either one after another ([`Self::sequential`]) or up to a number of
/// entries at the same time ([`Self::parallel`]). Entries are tests ([`Self::with`]) and nested
/// groups ([`Self::with_nested`]), so any mix of sequential and parallel execution can be expressed:
/// a sequential group of groups runs stages one after another, and a sequential group inside a
/// parallel group keeps its tests from running at the same time while other tests run alongside.
/// The tests of a logical [`TestGroup`] ([`Self::with_test_group`]) are entries of their own.
///
/// Entries start in the order they were added. Every test runs in a fresh browser session, or in
/// the reset session of an earlier test (see [`crate::SessionReuse`]). How
/// failures affect the run is decided by [`crate::BrowserTestRunner::with_failure_policy`]. With
/// [`crate::FailurePolicy::RunAll`], every test runs. With [`crate::FailurePolicy::FailFast`], no
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
///     .with_nested(
///         BrowserTests::parallel(Parallelism::from_env()?.unwrap_or(Parallelism::parallel(4)))
///             .with(Buttons)
///             .with(Tables)
///             // These two share server state, so they must not run at the same time.
///             .with_nested(BrowserTests::sequential().with(CreateUser).with(DeleteUser)),
///     )
///     .with_nested(
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
    pub(crate) parallelism: Parallelism,
    pub(crate) name: Option<String>,
    pub(crate) run_always: bool,
    pub(crate) entries: Vec<BrowserTestEntry<Context, TestError>>,
}

/// A logical group of tests, e.g. the tests of one component, for selecting them by name.
///
/// [`TestFilter::with_group`](crate::TestFilter::with_group) and `BROWSER_TEST_GROUP` select the
/// tests of logical groups. A logical group does not change how its tests run: added with
/// [`BrowserTests::with_test_group`], they run like tests added to that [`BrowserTests`] one by
/// one, in its parallelism limit. Use nested [`BrowserTests`] to decide how tests run.
///
/// A test's group is independent of its Rust module and does not change its name. Test records
/// carry the group's name (see [`BrowserTestRecord::group`](crate::BrowserTestRecord::group)).
///
/// # Examples
///
/// ```
/// use browser_test::{BrowserTests, Parallelism, TestGroup, browser_test};
/// use rootcause::Report;
///
/// #[browser_test]
/// async fn presses(_base_url: &str) -> Result<(), Report> { Ok(()) }
/// #[browser_test]
/// async fn presses_with_keyboard(_base_url: &str) -> Result<(), Report> { Ok(()) }
///
/// let tests: BrowserTests<str> = BrowserTests::parallel(Parallelism::parallel(4))
///     .with_test_group(TestGroup::new("button").with(Presses).with(PressesWithKeyboard));
/// ```
pub struct TestGroup<Context = (), TestError = rootcause::markers::Dynamic>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    pub(crate) name: String,
    pub(crate) tests: Vec<Box<dyn BrowserTest<Context, TestError>>>,
}

impl<Context, TestError> TestGroup<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    /// An empty group named `name`, which [`TestFilter::with_group`](crate::TestFilter::with_group)
    /// selects.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tests: Vec::new(),
        }
    }

    /// Add a test.
    #[must_use]
    pub fn with(mut self, test: impl BrowserTest<Context, TestError> + 'static) -> Self {
        self.tests.push(Box::new(test));
        self
    }
}

impl<Context, TestError> fmt::Debug for TestGroup<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestGroup")
            .field("name", &self.name)
            .field(
                "tests",
                &self
                    .tests
                    .iter()
                    .map(|test| DebugName(test.as_ref()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// An entry of a [`BrowserTests`] group.
pub(crate) enum BrowserTestEntry<Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    Test(Box<dyn BrowserTest<Context, TestError>>),
    Group(BrowserTests<Context, TestError>),
    TestGroup(TestGroup<Context, TestError>),
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

    /// Add a nested group, which runs its entries as it says, as one entry of this group.
    ///
    /// Not to be confused with [`Self::with_test_group`], which adds the tests of a logical group
    /// as entries of this group.
    #[must_use]
    pub fn with_nested(mut self, group: Self) -> Self {
        self.entries.push(BrowserTestEntry::Group(group));
        self
    }

    /// Add the tests of a logical group, to run like tests added with [`Self::with`], in order.
    ///
    /// The group does not change how its tests run. It only makes them selectable by its name,
    /// see [`TestGroup`].
    #[must_use]
    pub fn with_test_group(mut self, group: TestGroup<Context, TestError>) -> Self {
        self.entries.push(BrowserTestEntry::TestGroup(group));
        self
    }

    /// Keep only the tests `filter` selects, e.g. [`TestFilter::from_env`](crate::TestFilter::from_env).
    ///
    /// Nested groups keep how they run, and empty ones are removed. Add tests that must always run,
    /// e.g. checks of the whole run, after filtering: [`Self::run_always`] does not exempt tests
    /// from filters.
    #[must_use]
    pub fn filter(self, filter: &TestFilter) -> Self {
        filter.apply(self)
    }

    /// Keep only the tests of the logical [`TestGroup`]s whose name `include` accepts.
    ///
    /// Tests outside of logical groups are removed. The names of nested [`BrowserTests`] are not
    /// matched: they keep how they run, and empty ones are removed. Groups marked
    /// [`Self::run_always`] are filtered too: add tests that must always run after filtering.
    #[must_use]
    pub fn filter_groups(mut self, mut include: impl FnMut(&str) -> bool) -> Self {
        self.retain_groups(&mut include);
        self
    }

    fn retain_groups(&mut self, include: &mut impl FnMut(&str) -> bool) {
        self.entries.retain_mut(|entry| match entry {
            BrowserTestEntry::Test(_) => false,
            BrowserTestEntry::TestGroup(group) => include(&group.name) && !group.tests.is_empty(),
            BrowserTestEntry::Group(group) => {
                group.retain_groups(include);
                !group.is_empty()
            }
        });
    }

    /// Keep only the tests whose name `include` accepts, in this group and nested ones.
    ///
    /// A test whose [`BrowserTest::name`] panics is kept, so that the run reports it as failed.
    /// Nested groups keep how they run, and empty ones are removed. Groups marked
    /// [`Self::run_always`] are filtered too: add tests that must always run after filtering.
    #[must_use]
    pub fn filter_tests(mut self, mut include: impl FnMut(&str) -> bool) -> Self {
        self.retain_tests(&mut include);
        self
    }

    fn retain_tests(&mut self, include: &mut impl FnMut(&str) -> bool) {
        self.entries.retain_mut(|entry| match entry {
            BrowserTestEntry::Test(test) => includes(include, test.as_ref()),
            BrowserTestEntry::TestGroup(group) => {
                group.tests.retain(|test| includes(include, test.as_ref()));
                !group.tests.is_empty()
            }
            BrowserTestEntry::Group(group) => {
                group.retain_tests(include);
                !group.is_empty()
            }
        });
    }

    /// Whether this group contains no test, directly or in nested groups.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|entry| match entry {
            BrowserTestEntry::Test(_) => false,
            BrowserTestEntry::Group(group) => group.is_empty(),
            BrowserTestEntry::TestGroup(group) => group.tests.is_empty(),
        })
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
            Self::Test(test) => DebugName(test.as_ref()).fmt(f),
            Self::Group(group) => group.fmt(f),
            Self::TestGroup(group) => group.fmt(f),
        }
    }
}

/// The name of `test`, or `None` if reading it panics.
fn name_of<Context, TestError>(test: &dyn BrowserTest<Context, TestError>) -> Option<Cow<'_, str>>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    std::panic::catch_unwind(AssertUnwindSafe(|| test.name())).ok()
}

/// Whether `include` accepts the name of `test`. A test whose name panics is kept, so that the
/// run reports it as failed.
fn includes<Context, TestError>(
    include: &mut impl FnMut(&str) -> bool,
    test: &dyn BrowserTest<Context, TestError>,
) -> bool
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    name_of(test).is_none_or(|name| include(&name))
}

/// A test's name in `Debug` output, also if reading it panics.
struct DebugName<'a, Context: Sync + ?Sized, TestError: ?Sized>(
    &'a dyn BrowserTest<Context, TestError>,
);

impl<Context, TestError> fmt::Debug for DebugName<'_, Context, TestError>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match name_of(self.0) {
            Some(name) => name.fmt(f),
            None => f.write_str("<name() panicked>"),
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

    struct UnnamedTest;

    #[async_trait::async_trait]
    impl BrowserTest for UnnamedTest {
        fn name(&self) -> Cow<'_, str> {
            panic!("no name")
        }

        async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
            Ok(())
        }
    }

    #[test]
    fn tests_whose_name_panics_survive_filters_and_debug_output() {
        let tests = BrowserTests::sequential()
            .with(NamedTest("menu"))
            .with(UnnamedTest)
            .with_test_group(TestGroup::new("group").with(UnnamedTest))
            .filter_tests(|name| name.contains("checkout"));

        // Kept, so that the run reports them as failed.
        assert_eq!(
            format!("{tests:?}"),
            "BrowserTests { parallelism: Parallelism { max_parallel_tests: 1 }, name: None, \
             run_always: false, entries: [<name() panicked>, TestGroup { name: \"group\", \
             tests: [<name() panicked>] }] }"
        );
    }

    #[test]
    fn browser_tests_debug_prints_the_tree_with_test_names() {
        let tests = BrowserTests::sequential()
            .with(NamedTest("opens home page"))
            .with_nested(
                BrowserTests::sequential()
                    .named("nested")
                    .with(NamedTest("search works")),
            );

        assert_eq!(
            format!("{tests:?}"),
            concat!(
                r#"BrowserTests { parallelism: Parallelism { max_parallel_tests: 1 }, name: None, "#,
                r#"run_always: false, entries: ["opens home page", BrowserTests { parallelism: "#,
                r#"Parallelism { max_parallel_tests: 1 }, name: Some("nested"), run_always: false, "#,
                r#"entries: ["search works"] }] }"#,
            )
        );
    }

    #[test]
    fn groups_without_tests_are_empty() {
        let empty = BrowserTests::<()>::sequential().with_nested(BrowserTests::sequential());
        assert!(empty.is_empty());
        assert!(!empty.with(NamedTest("test")).is_empty());
        assert!(
            BrowserTests::<()>::sequential()
                .with_test_group(TestGroup::new("empty"))
                .is_empty()
        );
    }

    fn test_names(tests: &BrowserTests) -> Vec<String> {
        tests
            .entries
            .iter()
            .flat_map(|entry| match entry {
                BrowserTestEntry::Test(test) => vec![test.name().into_owned()],
                BrowserTestEntry::TestGroup(group) => group
                    .tests
                    .iter()
                    .map(|test| test.name().into_owned())
                    .collect(),
                BrowserTestEntry::Group(group) => test_names(group),
            })
            .collect()
    }

    fn suite() -> BrowserTests {
        BrowserTests::parallel(Parallelism::parallel(2))
            .with(NamedTest("ungrouped hover"))
            .with_test_group(
                TestGroup::new("buttons")
                    .with(NamedTest("atom::hover"))
                    .with(NamedTest("hook::press")),
            )
            .with_nested(
                BrowserTests::sequential()
                    .named("stage")
                    .run_always()
                    .with_test_group(
                        TestGroup::new("buttons-extra").with(NamedTest("other::hover")),
                    )
                    .with_test_group(TestGroup::new("focus").with(NamedTest("keyboard::focus"))),
            )
    }

    #[test]
    fn selection_combines_exact_groups_with_name_substrings_in_registration_order() {
        assert_eq!(
            test_names(&suite().filter(&TestFilter::new())),
            [
                "ungrouped hover",
                "atom::hover",
                "hook::press",
                "other::hover",
                "keyboard::focus"
            ]
        );
        assert_eq!(
            test_names(&suite().filter(&TestFilter::new().with_name_containing("hover"))),
            ["ungrouped hover", "atom::hover", "other::hover"]
        );
        assert_eq!(
            test_names(&suite().filter(&TestFilter::new().with_group("buttons"))),
            ["atom::hover", "hook::press"]
        );
        assert_eq!(
            test_names(
                &suite().filter(
                    &TestFilter::new()
                        .with_group("buttons")
                        .with_group("focus")
                        .with_name_containing("press")
                        .with_name_containing("focus")
                )
            ),
            ["hook::press", "keyboard::focus"]
        );
        assert!(
            suite()
                .filter(&TestFilter::new().with_group("missing"))
                .is_empty()
        );
        assert!(
            suite()
                .filter(
                    &TestFilter::new()
                        .with_group("buttons")
                        .with_name_containing("absent")
                )
                .is_empty()
        );
    }

    #[test]
    fn selection_preserves_execution_policy_and_allows_unconditional_checks() {
        let tests = suite().filter(&TestFilter::new().with_group("focus"));
        assert_eq!(tests.parallelism, Parallelism::parallel(2));
        let BrowserTestEntry::Group(stage) = &tests.entries[0] else {
            panic!("expected stage")
        };
        assert_eq!(stage.name.as_deref(), Some("stage"));
        assert_eq!(stage.parallelism, Parallelism::sequential());
        assert!(stage.run_always);
        let tests = tests.with_nested(
            BrowserTests::sequential()
                .run_always()
                .with(NamedTest("after all")),
        );
        assert_eq!(test_names(&tests), ["keyboard::focus", "after all"]);
    }

    #[test]
    fn async_functions_accept_borrowed_contexts_and_explicit_names() {
        struct Page<'a>(&'a str);
        async fn reads_page(page: &Page<'_>) -> Result<(), Report> {
            assert_eq!(page.0, "page");
            Ok(())
        }
        fn accepts_page_test(_test: impl for<'a> BrowserTest<Page<'a>>) {}
        accepts_page_test(reads_page);
        let test = reads_page.named("explicit name");
        assert_eq!(test.name(), "explicit name");
        accepts_page_test(test);
    }

    #[test]
    fn naming_preserves_description_and_session_settings() {
        use std::time::Duration;

        use crate::{ElementQueryWait, Timeouts};

        struct Settings;
        #[async_trait]
        impl BrowserTest for Settings {
            fn description(&self) -> Option<Cow<'_, str>> {
                Some("described".into())
            }
            fn session_settings(&self) -> SessionSettings {
                SessionSettings::new()
                    .with_timeouts(Timeouts::new().with_implicit_wait(Duration::ZERO))
                    .with_element_query_wait(ElementQueryWait::new(
                        Duration::from_secs(1),
                        Duration::from_millis(10),
                    ))
                    .with_fresh_session(true)
            }
            async fn run(&self, _: &WebDriver, (): &()) -> Result<(), Report> {
                Ok(())
            }
        }
        let test = Settings.named("renamed");
        assert_eq!(test.name(), "renamed");
        assert_eq!(test.description(), Settings.description());
        assert_eq!(test.session_settings(), Settings.session_settings());
    }
}

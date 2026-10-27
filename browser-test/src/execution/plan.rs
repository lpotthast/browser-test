//! The test tree of a run: its tests, numbered in definition order and completed with the
//! runner's settings, and the suites they run in.

use std::{borrow::Cow, collections::BTreeMap, panic::AssertUnwindSafe, time::Duration};

use crate::{
    BrowserTest, BrowserTestRecord, ElementQueryWait, Parallelism, SessionTiming, TestOutcome,
    TestSuite, Timeouts, failure_report::panic_payload_message, test_case::SuiteEntry,
    test_settings::TestDefaults,
};

/// Where a test sits in the test tree.
#[derive(Clone, Default)]
pub(super) struct Placement {
    /// The names of the named suites it runs in, outermost first, joined with ` / `.
    pub(super) path: Option<String>,
    /// Whether it runs even after a failure stopped the run (see [`TestSuite::run_always`]).
    pub(super) run_always: bool,
}

/// A test of the run, numbered in definition order, with its metadata.
pub(super) struct QueuedTest<Context>
where
    Context: Sync + ?Sized,
{
    pub(super) index: usize,
    /// The test's name, or a placeholder if reading it panicked.
    pub(super) name: String,
    pub(super) placement: Placement,
    /// See [`BrowserTest::tags`]. Empty if reading them panicked.
    pub(super) tags: Vec<String>,
    /// The test, ready to run, or the message of the panic its metadata methods raised. Such a
    /// test fails without running.
    pub(super) prepared: Result<PreparedTest<Context>, String>,
}

impl<Context> QueuedTest<Context>
where
    Context: Sync + ?Sized,
{
    /// The ready test, which only tests assigned a session are.
    pub(super) fn ready(&self) -> &PreparedTest<Context> {
        self.prepared
            .as_ref()
            .unwrap_or_else(|_| unreachable!("only ready tests are assigned to sessions"))
    }

    /// The record of this test, without the steps and the teardown of its session, which the
    /// session worker of a test that ran adds.
    pub(super) fn record(
        &self,
        outcome: TestOutcome,
        session: Option<SessionTiming>,
        body: Option<Duration>,
    ) -> BrowserTestRecord {
        BrowserTestRecord {
            index: self.index,
            name: self.name.clone(),
            outcome,
            path: self.placement.path.clone(),
            tags: self.tags.clone(),
            session,
            body,
            teardown: None,
            steps: BTreeMap::new(),
        }
    }
}

/// What a test whose metadata was read needs to run.
pub(super) struct PreparedTest<Context>
where
    Context: Sync + ?Sized,
{
    /// The test's timeouts, completed with the runner's.
    pub(super) timeouts: Timeouts,
    pub(super) element_query_wait: Option<ElementQueryWait>,
    /// Whether the test needs a session no test ran in (see
    /// [`TestSettings::fresh_session`](crate::TestSettings::fresh_session)).
    pub(super) fresh_session: bool,
    /// How long its body may run: its own time limit, else the runner's.
    pub(super) time_limit: Duration,
    pub(super) test: Box<dyn BrowserTest<Context>>,
}

impl<Context> PreparedTest<Context>
where
    Context: Sync + ?Sized,
{
    /// Whether the test takes its session from the pool, rather than getting one created with
    /// its own settings.
    pub(super) fn uses_pool(&self, default_wait: Option<ElementQueryWait>) -> bool {
        self.element_query_wait == default_wait
    }
}

/// A suite of the test tree, with its tests referenced by index.
pub(super) struct Suite {
    /// Definition order, for sorting suite records.
    pub(super) id: usize,
    /// The suite's name path, if the suite is named.
    pub(super) name: Option<String>,
    pub(super) parallelism: Parallelism,
    pub(super) children: Vec<Node>,
}

pub(super) enum Node {
    Test(usize),
    Suite(Suite),
}

impl Suite {
    /// How many tests of this suite can run at the same time.
    pub(super) fn max_concurrency(&self) -> usize {
        let mut children: Vec<usize> = self
            .children
            .iter()
            .map(|child| match child {
                Node::Test(_) => 1,
                Node::Suite(suite) => suite.max_concurrency(),
            })
            .collect();
        children.sort_unstable_by(|a, b| b.cmp(a));
        children
            .into_iter()
            .take(self.parallelism.max_parallel_tests().get())
            .sum::<usize>()
            .max(1)
    }
}

/// Number the tests of `suite` in definition order, complete them with the runner's `defaults`,
/// and build the tree they run in.
pub(super) fn plan<Context>(
    suite: TestSuite<Context>,
    defaults: TestDefaults,
) -> (Suite, Vec<QueuedTest<Context>>)
where
    Context: Sync + ?Sized,
{
    let mut tests = Vec::new();
    let root = plan_suite(suite, &Placement::default(), &mut tests, &mut 0, defaults);
    (root, tests)
}

/// Plan `suite`, placed in `parent`, adding its tests to `tests`.
fn plan_suite<Context>(
    suite: TestSuite<Context>,
    parent: &Placement,
    tests: &mut Vec<QueuedTest<Context>>,
    next_suite: &mut usize,
    defaults: TestDefaults,
) -> Suite
where
    Context: Sync + ?Sized,
{
    let TestSuite {
        parallelism,
        name,
        run_always,
        entries,
    } = suite;
    let named = name.is_some();
    let path = match (parent.path.as_deref(), name) {
        (Some(parent), Some(name)) => Some(format!("{parent} / {name}")),
        (None, Some(name)) => Some(name),
        (parent, None) => parent.map(str::to_owned),
    };
    let placement = Placement {
        path,
        run_always: parent.run_always || run_always,
    };
    let id = *next_suite;
    *next_suite += 1;
    let mut children = Vec::new();
    for entry in entries {
        match entry {
            SuiteEntry::Test(test) => {
                let index = tests.len();
                tests.push(plan_test(index, test, placement.clone(), defaults));
                children.push(Node::Test(index));
            }
            SuiteEntry::Suite(suite) => children.push(Node::Suite(plan_suite(
                suite, &placement, tests, next_suite, defaults,
            ))),
        }
    }
    Suite {
        id,
        name: if named { placement.path } else { None },
        parallelism,
        children,
    }
}

/// Read the metadata of `test`, the test at `index`, and complete its settings with `defaults`.
fn plan_test<Context>(
    index: usize,
    test: Box<dyn BrowserTest<Context>>,
    placement: Placement,
    defaults: TestDefaults,
) -> QueuedTest<Context>
where
    Context: Sync + ?Sized,
{
    // What was read before a metadata method panicked is kept, with a fallback for the name.
    let mut name = format!("unnamed test at index {index}");
    let mut tags = Vec::new();
    let settings = std::panic::catch_unwind(AssertUnwindSafe(|| {
        name = test.name().into_owned();
        tags = test.tags().into_iter().map(Cow::into_owned).collect();
        test.settings()
    }));
    let prepared = match settings {
        Ok(settings) => Ok(PreparedTest {
            timeouts: settings.timeouts().or(defaults.timeouts),
            element_query_wait: settings
                .element_query_wait()
                .or(defaults.element_query_wait),
            fresh_session: settings.fresh_session(),
            time_limit: settings.time_limit().unwrap_or(defaults.time_limit),
            test,
        }),
        Err(payload) => {
            let panic_message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{name}' panicked: {panic_message}");
            Err(panic_message)
        }
    };
    QueuedTest {
        index,
        name,
        placement,
        tags,
        prepared,
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;
    use crate::{
        TestSettings,
        test_support::{Metadata, StubTest},
    };

    #[test]
    fn tests_keep_their_names_tags_and_paths() {
        let tests = TestSuite::parallel(Parallelism::parallel(2))
            .named("ui")
            .with(StubTest::new("a").with_tags(&["one"]))
            .with(StubTest::new("b").with_tags(&["one", "two"]))
            .with(StubTest::new("c"));
        let (_root, tests) = plan(tests, TestDefaults::new());
        let metadata: Vec<_> = tests
            .iter()
            .map(|test| {
                (
                    test.index,
                    test.name.as_str(),
                    test.placement.path.as_deref(),
                    test.tags.clone(),
                    test.prepared.is_ok(),
                )
            })
            .collect();
        assert_eq!(
            metadata,
            [
                (0, "a", Some("ui"), vec!["one".to_owned()], true),
                (
                    1,
                    "b",
                    Some("ui"),
                    vec!["one".to_owned(), "two".to_owned()],
                    true
                ),
                (2, "c", Some("ui"), Vec::new(), true),
            ]
        );
    }

    #[test]
    fn a_test_whose_metadata_panics_keeps_what_was_read_before() {
        let tests = TestSuite::sequential()
            .with(StubTest::new("named").panicking_in(Metadata::Name))
            .with(
                StubTest::new("tagged")
                    .with_tags(&["one"])
                    .panicking_in(Metadata::Tags),
            )
            .with(
                StubTest::new("settings")
                    .with_tags(&["one"])
                    .panicking_in(Metadata::Settings),
            );
        let (_root, tests) = plan(tests, TestDefaults::new());
        let metadata: Vec<_> = tests
            .iter()
            .map(|test| {
                (
                    test.name.as_str(),
                    test.tags.clone(),
                    test.prepared.as_ref().err().cloned(),
                )
            })
            .collect();
        assert_eq!(
            metadata,
            [
                (
                    "unnamed test at index 0",
                    Vec::new(),
                    Some("named panics".to_owned())
                ),
                ("tagged", Vec::new(), Some("tagged panics".to_owned())),
                (
                    "settings",
                    vec!["one".to_owned()],
                    Some("settings panics".to_owned())
                ),
            ]
        );
        let record = tests[2].record(TestOutcome::Panicked, None, None);
        assert_eq!(
            (record.index, record.name.as_str(), record.tags),
            (2, "settings", vec!["one".to_owned()])
        );
    }

    #[test]
    fn nested_suites_join_their_names_and_inherit_run_always() {
        let tests = TestSuite::sequential()
            .named("checkout")
            .with(StubTest::new("a"))
            .with_nested(
                TestSuite::sequential()
                    .named("final checks")
                    .run_always()
                    .with_nested(TestSuite::sequential().with(StubTest::new("b"))),
            );
        let (_root, tests) = plan(tests, TestDefaults::new());
        let placements: Vec<_> = tests
            .iter()
            .map(|test| (test.placement.path.as_deref(), test.placement.run_always))
            .collect();
        assert_eq!(
            placements,
            [
                (Some("checkout"), false),
                (Some("checkout / final checks"), true),
            ]
        );
    }

    mod plan_test {
        use super::*;

        fn prepared(test: TestSettings, runner: TestDefaults) -> PreparedTest<()> {
            let test = StubTest::new("test").with_settings(test);
            let Ok(prepared) = plan_test(0, Box::new(test), Placement::default(), runner).prepared
            else {
                panic!("the settings are readable")
            };
            prepared
        }

        #[test]
        fn test_timeouts_override_the_runners_one_by_one() {
            let runner = TestDefaults {
                timeouts: Timeouts::new()
                    .with_script(Duration::from_secs(10))
                    .with_implicit_wait(Duration::ZERO),
                ..TestDefaults::new()
            };
            let test = TestSettings::new()
                .with_timeouts(Timeouts::new().with_script(Duration::from_secs(5)));

            assert_that!(prepared(test, runner).timeouts).is_equal_to(
                Timeouts::new()
                    .with_script(Duration::from_secs(5))
                    .with_implicit_wait(Duration::ZERO),
            );
            assert_that!(prepared(TestSettings::new(), TestDefaults::new()).timeouts)
                .is_equal_to(Timeouts::new());
        }

        #[test]
        fn test_element_query_wait_overrides_the_runners() {
            let runner_wait =
                ElementQueryWait::new(Duration::from_secs(10), Duration::from_secs(1));
            let test_wait =
                ElementQueryWait::new(Duration::from_secs(5), Duration::from_millis(500));
            let runner = TestDefaults {
                element_query_wait: Some(runner_wait),
                ..TestDefaults::new()
            };

            let overriding = prepared(
                TestSettings::new().with_element_query_wait(test_wait),
                runner,
            );
            assert_that!(overriding.element_query_wait).is_equal_to(Some(test_wait));
            assert_that!(overriding.uses_pool(Some(runner_wait))).is_false();

            let inheriting = prepared(TestSettings::new(), runner);
            assert_that!(inheriting.element_query_wait).is_equal_to(Some(runner_wait));
            assert_that!(inheriting.uses_pool(Some(runner_wait))).is_true();

            assert_that!(prepared(TestSettings::new(), TestDefaults::new()).element_query_wait)
                .is_none();
        }

        #[test]
        fn fresh_session_is_the_tests_own() {
            let test = TestSettings::new().with_fresh_session(true);
            assert_that!(prepared(test, TestDefaults::new()).fresh_session).is_true();
        }

        #[test]
        fn a_tests_time_limit_overrides_the_runners() {
            let runner = TestDefaults {
                time_limit: Duration::from_secs(60),
                ..TestDefaults::new()
            };
            let limited = prepared(
                TestSettings::new().with_time_limit(Duration::from_secs(5)),
                runner,
            );
            assert_that!(limited.time_limit).is_equal_to(Duration::from_secs(5));
            assert_that!(prepared(TestSettings::new(), runner).time_limit)
                .is_equal_to(Duration::from_secs(60));
        }
    }
}

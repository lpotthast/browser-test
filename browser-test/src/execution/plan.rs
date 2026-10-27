//! The test tree of a run: its tests, numbered in definition order and completed with the
//! runner's settings, and the groups they run in.

use std::{panic::AssertUnwindSafe, time::Duration};

use crate::{
    BrowserTest, BrowserTests, ElementQueryWait, Parallelism, Timeouts,
    failure_report::panic_payload_message, test_case::BrowserTestEntry,
    test_settings::TestDefaults,
};

/// Where a test sits in the test tree.
#[derive(Clone, Default)]
pub(super) struct Placement {
    /// The names of the named groups it runs in, outermost first, joined with ` / `.
    pub(super) path: Option<String>,
    /// The name of its [`TestGroup`](crate::TestGroup).
    pub(super) group: Option<String>,
    /// Whether it runs even after a failure stopped the run (see [`BrowserTests::run_always`]).
    pub(super) run_always: bool,
}

/// A test whose metadata (name, settings) was read successfully.
pub(super) struct PreparedTest<Context>
where
    Context: Sync + ?Sized,
{
    pub(super) index: usize,
    pub(super) name: String,
    pub(super) placement: Placement,
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

pub(super) enum QueuedTest<Context>
where
    Context: Sync + ?Sized,
{
    Ready(PreparedTest<Context>),
    /// Reading the test's metadata panicked. The test fails without running.
    Broken {
        index: usize,
        name: String,
        placement: Placement,
        panic_message: String,
    },
}

impl<Context> QueuedTest<Context>
where
    Context: Sync + ?Sized,
{
    pub(super) fn placement(&self) -> &Placement {
        match self {
            Self::Ready(test) => &test.placement,
            Self::Broken { placement, .. } => placement,
        }
    }
}

/// A group of the test tree, with its tests referenced by index.
pub(super) struct Group {
    /// Definition order, for sorting group records.
    pub(super) id: usize,
    /// The group's name path, if the group is named.
    pub(super) name: Option<String>,
    pub(super) parallelism: Parallelism,
    pub(super) children: Vec<Node>,
}

pub(super) enum Node {
    Test(usize),
    Group(Group),
}

impl Group {
    /// How many tests of this group can run at the same time.
    pub(super) fn max_concurrency(&self) -> usize {
        let mut children: Vec<usize> = self
            .children
            .iter()
            .map(|child| match child {
                Node::Test(_) => 1,
                Node::Group(group) => group.max_concurrency(),
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

/// Number the tests of `tests`, placed in `parent`, in definition order into `prepared` and build
/// the tree.
pub(super) fn prepare_group<Context>(
    tests: BrowserTests<Context>,
    parent: &Placement,
    prepared: &mut Vec<QueuedTest<Context>>,
    next_group: &mut usize,
    defaults: TestDefaults,
) -> Group
where
    Context: Sync + ?Sized,
{
    let BrowserTests {
        parallelism,
        name,
        run_always,
        entries,
    } = tests;
    let named = name.is_some();
    let path = match (parent.path.as_deref(), name) {
        (Some(parent), Some(name)) => Some(format!("{parent} / {name}")),
        (None, Some(name)) => Some(name),
        (parent, None) => parent.map(str::to_owned),
    };
    let placement = Placement {
        path,
        group: None,
        run_always: parent.run_always || run_always,
    };
    let id = *next_group;
    *next_group += 1;
    let mut children = Vec::new();
    for entry in entries {
        match entry {
            BrowserTestEntry::Test(test) => {
                let index = prepared.len();
                prepared.push(prepare_test(index, test, placement.clone(), defaults));
                children.push(Node::Test(index));
            }
            BrowserTestEntry::Group(group) => children.push(Node::Group(prepare_group(
                group, &placement, prepared, next_group, defaults,
            ))),
            BrowserTestEntry::TestGroup(group) => {
                let placement = Placement {
                    group: Some(group.name),
                    ..placement.clone()
                };
                for test in group.tests {
                    let index = prepared.len();
                    prepared.push(prepare_test(index, test, placement.clone(), defaults));
                    children.push(Node::Test(index));
                }
            }
        }
    }
    Group {
        id,
        name: if named { placement.path } else { None },
        parallelism,
        children,
    }
}

fn prepare_test<Context>(
    index: usize,
    test: Box<dyn BrowserTest<Context>>,
    placement: Placement,
    defaults: TestDefaults,
) -> QueuedTest<Context>
where
    Context: Sync + ?Sized,
{
    // Used in the panic report. We need a fallback for the case that `.name()` panics.
    let mut name = format!("unnamed test at index {index}");
    let settings = std::panic::catch_unwind(AssertUnwindSafe(|| {
        name = test.name().into_owned();
        test.settings()
    }));
    match settings {
        Ok(settings) => QueuedTest::Ready(PreparedTest {
            index,
            name,
            placement,
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
            QueuedTest::Broken {
                index,
                name,
                placement,
                panic_message,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;
    use rootcause::Report;
    use thirtyfour::WebDriver;

    use super::*;
    use crate::TestSettings;

    #[test]
    fn logical_groups_share_parent_concurrency_and_keep_their_names_apart_from_the_path() {
        async fn passes((): &()) -> Result<(), Report> {
            Ok(())
        }
        let tests = BrowserTests::parallel(Parallelism::parallel(2))
            .named("ui")
            .with_test_group(
                crate::TestGroup::new("one")
                    .with(passes.named("a"))
                    .with(passes.named("b")),
            )
            .with_test_group(
                crate::TestGroup::new("two")
                    .with(passes.named("c"))
                    .with(passes.named("d")),
            );
        let mut prepared = Vec::new();
        let defaults = TestDefaults::new();
        let root = prepare_group(
            tests,
            &Placement::default(),
            &mut prepared,
            &mut 0,
            defaults,
        );
        assert_eq!(root.max_concurrency(), 2);
        assert_eq!(root.children.len(), 4);
        assert!(
            root.children
                .iter()
                .all(|child| matches!(child, Node::Test(_)))
        );
        let metadata: Vec<_> = prepared
            .iter()
            .map(|test| match test {
                QueuedTest::Ready(test) => (
                    test.index,
                    test.name.as_str(),
                    test.placement.path.as_deref(),
                    test.placement.group.as_deref(),
                ),
                QueuedTest::Broken { .. } => panic!("metadata must be valid"),
            })
            .collect();
        assert_eq!(
            metadata,
            [
                (0, "a", Some("ui"), Some("one")),
                (1, "b", Some("ui"), Some("one")),
                (2, "c", Some("ui"), Some("two")),
                (3, "d", Some("ui"), Some("two")),
            ]
        );
    }

    #[test]
    fn nested_groups_join_their_names_and_inherit_run_always() {
        async fn passes((): &()) -> Result<(), Report> {
            Ok(())
        }
        let tests = BrowserTests::sequential()
            .named("checkout")
            .with(passes.named("a"))
            .with_nested(
                BrowserTests::sequential()
                    .named("final checks")
                    .run_always()
                    .with_nested(BrowserTests::sequential().with(passes.named("b"))),
            );
        let mut prepared = Vec::new();
        let defaults = TestDefaults::new();
        prepare_group(
            tests,
            &Placement::default(),
            &mut prepared,
            &mut 0,
            defaults,
        );
        let placements: Vec<_> = prepared
            .iter()
            .map(|test| {
                let placement = test.placement();
                (placement.path.as_deref(), placement.run_always)
            })
            .collect();
        assert_eq!(
            placements,
            [
                (Some("checkout"), false),
                (Some("checkout / final checks"), true),
            ]
        );
    }

    mod prepare_test {
        use super::*;

        struct SettingsTest(TestSettings);

        #[async_trait::async_trait]
        impl BrowserTest for SettingsTest {
            fn settings(&self) -> TestSettings {
                self.0
            }

            async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
                Ok(())
            }
        }

        fn prepared(test: TestSettings, runner: TestDefaults) -> PreparedTest<()> {
            match prepare_test(
                0,
                Box::new(SettingsTest(test)),
                Placement::default(),
                runner,
            ) {
                QueuedTest::Ready(test) => test,
                QueuedTest::Broken { .. } => panic!("the settings are readable"),
            }
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

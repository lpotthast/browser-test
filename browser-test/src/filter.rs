//! Selecting the tests of a run by name and tag.

use std::{fmt, panic::AssertUnwindSafe};

use regex::Regex;

use crate::{BrowserTest, BrowserTestError, InvalidEnvVar, env::env_unicode_value};

/// Default environment variable read by [`TestFilter::from_env`] for the name pattern.
pub(crate) const DEFAULT_NAME_FILTER_ENV: &str = "BROWSER_TEST_NAME";

/// Default environment variable read by [`TestFilter::from_env`] for the tag pattern.
pub(crate) const DEFAULT_TAG_FILTER_ENV: &str = "BROWSER_TEST_TAG";

/// What the filter's environment variables hold.
const EXPECTED_PATTERN: &str = "a regular expression";

/// Selects tests by regular expressions matched against their name and [tags](BrowserTest::tags).
///
/// A test is selected if any of the name patterns matches its [name](BrowserTest::name), and any
/// of the tag patterns matches one of its tags. A filter without name patterns selects tests of any
/// name, and one without tag patterns selects tests with any tags, including untagged tests.
/// [`Self::new`] selects every test.
///
/// Patterns use the syntax of the [`regex`](https://docs.rs/regex) crate, and match anywhere in
/// the name or tag: `keyboard` selects `checkout::keyboard_navigation`, and `smoke` also selects
/// the tag `smoke-slow`. Anchor a pattern to match a prefix (`^checkout::`) or the whole name or
/// tag (`^smoke$`). Matching is case-sensitive, unless the pattern starts with `(?i)`.
///
/// Give the filter to the runner with
/// [`BrowserTestRunner::with_filter`](crate::BrowserTestRunner::with_filter), the preferred way to
/// select tests. Mark suites that must always run, e.g. checks of the whole run,
/// [`TestSuite::unfiltered`](crate::TestSuite::unfiltered). Selection is never implicit: without a
/// filter, a runner runs every test it is given, whatever the environment says.
/// [`TestSuite::filter`](crate::TestSuite::filter) applies a filter to a suite directly.
///
/// # Examples
///
/// Select the tests tagged exactly `button` or `focus` whose names contain `keyboard`:
///
/// ```
/// use browser_test::TestFilter;
///
/// let filter = TestFilter::new()
///     .with_tag("^(button|focus)$")
///     .with_name("keyboard");
/// ```
///
/// Let whoever runs the tests select them through `BROWSER_TEST_NAME` and `BROWSER_TEST_TAG`,
/// while checks of the whole run always run:
///
/// ```no_run
/// # use std::borrow::Cow;
/// # use browser_test::thirtyfour::WebDriver;
/// # use browser_test::{async_trait, BrowserTest};
/// # use rootcause::Report;
/// # struct ServerDidNotPanic;
/// # #[async_trait]
/// # impl BrowserTest for ServerDidNotPanic {
/// #     async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> { Ok(()) }
/// # }
/// # fn component_tests() -> browser_test::TestSuite { browser_test::TestSuite::sequential() }
/// # async fn run() -> Result<(), Report> {
/// use browser_test::{BrowserTestRunner, Cancellation, TestFilter, TestSuite};
///
/// let tests = TestSuite::sequential()
///     .with_nested(component_tests())
///     .with_nested(
///         TestSuite::sequential()
///             .run_always()
///             .unfiltered()
///             .with(ServerDidNotPanic),
///     );
///
/// BrowserTestRunner::new(Cancellation::on_shutdown_signals())
///     .with_filter(TestFilter::from_env()?)
///     .run(&(), tests)
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TestFilter {
    names: Vec<Pattern>,
    tags: Vec<Pattern>,
}

impl TestFilter {
    /// A filter selecting every test.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Also select tests whose name matches the regular expression `pattern` anywhere.
    ///
    /// The first pattern restricts the selection to matching names, every further one widens it.
    ///
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid regular expression. [`Self::try_with_name`] returns an
    /// error instead.
    #[must_use]
    #[track_caller]
    pub fn with_name(self, pattern: impl AsRef<str>) -> Self {
        valid(self.try_with_name(pattern))
    }

    /// Also select tests whose name matches the regular expression `pattern` anywhere, see
    /// [`Self::with_name`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPattern`] if `pattern` is not a valid regular expression.
    pub fn try_with_name(mut self, pattern: impl AsRef<str>) -> Result<Self, InvalidPattern> {
        self.names.push(Pattern::new(pattern.as_ref())?);
        Ok(self)
    }

    /// Also select tests with a tag that the regular expression `pattern` matches anywhere (see
    /// [`BrowserTest::tags`]).
    ///
    /// The first pattern restricts the selection to tests with a matching tag, every further one
    /// widens it. Untagged tests are no longer selected then.
    ///
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid regular expression. [`Self::try_with_tag`] returns an
    /// error instead.
    #[must_use]
    #[track_caller]
    pub fn with_tag(self, pattern: impl AsRef<str>) -> Self {
        valid(self.try_with_tag(pattern))
    }

    /// Also select tests with a tag that the regular expression `pattern` matches anywhere, see
    /// [`Self::with_tag`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPattern`] if `pattern` is not a valid regular expression.
    pub fn try_with_tag(mut self, pattern: impl AsRef<str>) -> Result<Self, InvalidPattern> {
        self.tags.push(Pattern::new(pattern.as_ref())?);
        Ok(self)
    }

    /// Read the name pattern from `BROWSER_TEST_NAME` and the tag pattern from
    /// `BROWSER_TEST_TAG`.
    ///
    /// See [`Self::with_name_pattern_from_env_var`] and [`Self::with_tag_pattern_from_env_var`].
    /// Unset variables select every test.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if a variable is not valid Unicode or not a valid regular
    /// expression.
    pub fn from_env() -> Result<Self, InvalidEnvVar> {
        Self::new()
            .with_name_pattern_from_env_var(DEFAULT_NAME_FILTER_ENV)?
            .with_tag_pattern_from_env_var(DEFAULT_TAG_FILTER_ENV)
    }

    /// Add the name pattern held by the variable `env_var`, as [`Self::with_name`] does.
    ///
    /// Whitespace around the pattern is ignored: an unset or empty variable adds none.
    /// `BROWSER_TEST_NAME='press|keyboard'` selects tests whose names contain `press` or
    /// `keyboard`. The variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not valid Unicode or not a valid regular
    /// expression.
    pub fn with_name_pattern_from_env_var(
        self,
        env_var: impl AsRef<str>,
    ) -> Result<Self, InvalidEnvVar> {
        let env_var = env_var.as_ref();
        match env_unicode_value(env_var, EXPECTED_PATTERN)? {
            Some(pattern) => self
                .try_with_name(pattern)
                .map_err(|error| error.in_env_var(env_var)),
            None => Ok(self),
        }
    }

    /// Add the tag pattern held by the variable `env_var`, as [`Self::with_tag`] does.
    ///
    /// Whitespace around the pattern is ignored: an unset or empty variable adds none.
    /// `BROWSER_TEST_TAG='^(button|focus)$'` selects the tests tagged `button` or `focus`. The
    /// variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not valid Unicode or not a valid regular
    /// expression.
    pub fn with_tag_pattern_from_env_var(
        self,
        env_var: impl AsRef<str>,
    ) -> Result<Self, InvalidEnvVar> {
        let env_var = env_var.as_ref();
        match env_unicode_value(env_var, EXPECTED_PATTERN)? {
            Some(pattern) => self
                .try_with_tag(pattern)
                .map_err(|error| error.in_env_var(env_var)),
            None => Ok(self),
        }
    }

    /// Whether this filter has no patterns, and so selects every test.
    #[must_use]
    pub fn selects_all(&self) -> bool {
        self.names.is_empty() && self.tags.is_empty()
    }

    /// Whether this filter selects `test`. Reads only the metadata it filters by, and selects a
    /// test whose name or tags panic, so that the run reports it as failed.
    pub(crate) fn selects<Context: Sync + ?Sized>(&self, test: &dyn BrowserTest<Context>) -> bool {
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            let name_matches = self.names.is_empty() || {
                let name = test.name();
                self.names.iter().any(|pattern| pattern.is_match(&name))
            };
            name_matches
                && (self.tags.is_empty()
                    || test
                        .tags()
                        .iter()
                        .any(|tag| self.tags.iter().any(|pattern| pattern.is_match(tag))))
        }))
        .unwrap_or(true)
    }

    /// The error of a run in which this filter selected none of the tests it could have.
    pub(crate) fn selected_no_test(&self) -> BrowserTestError {
        let patterns = |patterns: &[Pattern]| -> Vec<String> {
            patterns
                .iter()
                .map(|pattern| pattern.source.clone())
                .collect()
        };
        BrowserTestError::NoTestSelected {
            name_patterns: patterns(&self.names),
            tag_patterns: patterns(&self.tags),
        }
    }
}

/// Prints the patterns as written.
impl fmt::Debug for TestFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestFilter")
            .field("names", &self.names)
            .field("tags", &self.tags)
            .finish()
    }
}

/// The filter built by a `with_*` method that panics on invalid patterns.
#[track_caller]
fn valid(filter: Result<TestFilter, InvalidPattern>) -> TestFilter {
    match filter {
        Ok(filter) => filter,
        Err(error) => panic!("{error}"),
    }
}

/// A compiled pattern, compared and printed as written.
#[derive(Clone)]
struct Pattern {
    source: String,
    regex: Regex,
}

impl Pattern {
    fn new(source: &str) -> Result<Self, InvalidPattern> {
        Ok(Self {
            source: source.to_owned(),
            regex: compile(source)?,
        })
    }

    fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Pattern {}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}

fn compile(pattern: &str) -> Result<Regex, InvalidPattern> {
    Regex::new(pattern).map_err(|error| InvalidPattern {
        pattern: pattern.to_owned(),
        message: error.to_string(),
    })
}

/// A [`TestFilter`] pattern that is not a valid regular expression, returned by
/// [`TestFilter::try_with_name`] and [`TestFilter::try_with_tag`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid test filter pattern {pattern:?}: {message}")]
#[non_exhaustive]
pub struct InvalidPattern {
    /// The pattern.
    pub pattern: String,

    /// What is wrong with the pattern.
    pub message: String,
}

impl InvalidPattern {
    /// The error of the variable `name` holding this pattern.
    fn in_env_var(self, name: &str) -> InvalidEnvVar {
        InvalidEnvVar {
            name: name.to_owned(),
            value: self.pattern,
            expected: EXPECTED_PATTERN,
            details: Some(self.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;
    use crate::test_support::EnvVarGuard;

    #[test]
    fn environment_patterns_are_trimmed_and_empty_ones_ignored() {
        let names = EnvVarGuard::new(DEFAULT_NAME_FILTER_ENV);
        let tags = EnvVarGuard::new_unlocked(DEFAULT_TAG_FILTER_ENV);
        names.remove();
        tags.remove();
        assert_that!(TestFilter::from_env())
            .ok()
            .is_equal_to(TestFilter::new());
        assert_that!(TestFilter::from_env().unwrap().selects_all()).is_true();
        names.set(" \t ");
        tags.set("");
        assert_that!(TestFilter::from_env())
            .ok()
            .is_equal_to(TestFilter::new());
        names.set(" hover|press ");
        tags.set(" ^(button|focus)$\t");
        assert_that!(TestFilter::from_env()).ok().is_equal_to(
            TestFilter::new()
                .with_name("hover|press")
                .with_tag("^(button|focus)$"),
        );
    }

    #[test]
    fn environment_patterns_add_to_the_filter() {
        let names = EnvVarGuard::new("BROWSER_TEST_NAME_CUSTOM");
        names.set("press");
        assert_that!(
            TestFilter::new()
                .with_name("hover")
                .with_name_pattern_from_env_var("BROWSER_TEST_NAME_CUSTOM")
        )
        .ok()
        .is_equal_to(TestFilter::new().with_name("hover").with_name("press"));
    }

    #[test]
    fn patterns_match_anywhere_unless_anchored() {
        let pattern = Pattern::new("keyboard").unwrap();
        assert_that!(pattern.is_match("checkout::keyboard_navigation")).is_true();
        assert_that!(Pattern::new("smoke").unwrap().is_match("smoke-slow")).is_true();
        let anchored = Pattern::new("^(button|focus)$").unwrap();
        assert_that!(anchored.is_match("button")).is_true();
        assert_that!(anchored.is_match("focus")).is_true();
        assert_that!(anchored.is_match("buttons")).is_false();
    }

    #[test]
    fn filters_print_and_compare_patterns_as_written() {
        let filter = TestFilter::new()
            .with_name("^checkout::")
            .with_tag("button");
        assert_that!(format!("{filter:?}"))
            .is_equal_to(r#"TestFilter { names: ["^checkout::"], tags: ["button"] }"#);
        assert_that!(filter).is_not_equal_to(TestFilter::new().with_name("^checkout::"));
        // An empty name pattern selects every test, but is a pattern.
        assert_that!(TestFilter::new().with_name("").selects_all()).is_false();
    }

    #[test]
    fn invalid_patterns_are_errors() {
        assert_that!(TestFilter::new().try_with_tag("("))
            .err()
            .has_display_value(concat!(
                "invalid test filter pattern \"(\": ",
                "regex parse error:\n    (\n    ^\nerror: unclosed group",
            ));

        let names = EnvVarGuard::new("BROWSER_TEST_NAME_UNCLOSED");
        names.set(" ( ");
        assert_that!(
            TestFilter::new().with_name_pattern_from_env_var("BROWSER_TEST_NAME_UNCLOSED")
        )
        .err()
        .has_display_value(concat!(
            "environment variable BROWSER_TEST_NAME_UNCLOSED is set to \"(\", expected a ",
            "regular expression: regex parse error:\n    (\n    ^\nerror: unclosed group",
        ));
    }

    #[test]
    #[should_panic(expected = "invalid test filter pattern \"(\"")]
    fn with_methods_panic_on_invalid_patterns() {
        let _ = TestFilter::new().with_name("(");
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_filter_is_an_error() {
        use std::os::unix::ffi::OsStrExt;
        let names = EnvVarGuard::new("BROWSER_TEST_NAME_INVALID");
        names.set(std::ffi::OsStr::from_bytes(&[0xff]));
        assert_that!(TestFilter::new().with_name_pattern_from_env_var("BROWSER_TEST_NAME_INVALID"))
            .err()
            .has_display_value(concat!(
                "environment variable BROWSER_TEST_NAME_INVALID is set to \"\u{fffd}\", expected ",
                "a regular expression: the value is not valid Unicode",
            ));
    }
}

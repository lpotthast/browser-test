//! Selecting the tests of a run by name and tag.

use std::panic::AssertUnwindSafe;

use crate::{BrowserTest, InvalidEnvVar};

/// Default environment variable read by [`TestFilter::from_env`] for name substrings.
pub(crate) const DEFAULT_NAME_FILTER_ENV: &str = "BROWSER_TEST_FILTER";

/// Default environment variable read by [`TestFilter::from_env`] for tags.
pub(crate) const DEFAULT_TAG_FILTER_ENV: &str = "BROWSER_TEST_TAG";

/// Selects tests by name substring and by exact [tag](BrowserTest::tags).
///
/// A test is selected if its [name](BrowserTest::name) contains any of the name
/// substrings, and it has any of the tags. A filter without name substrings selects tests of any
/// name, and one without tags selects tests with any tags, including untagged tests.
/// [`Self::new`] selects every test. Matching is case-sensitive.
///
/// Apply a filter with [`TestSuite::filter`](crate::TestSuite::filter). Selection is never
/// implicit: a runner runs every test it is given, whatever the environment says.
///
/// # Examples
///
/// Select the tests tagged `button` or `focus` whose names contain `keyboard`:
///
/// ```
/// use browser_test::TestFilter;
///
/// let filter = TestFilter::new()
///     .with_tag("button")
///     .with_tag("focus")
///     .with_name_containing("keyboard");
/// ```
///
/// Let whoever runs the tests select them through `BROWSER_TEST_FILTER` and `BROWSER_TEST_TAG`,
/// and add checks of the whole run after filtering, so that they always run:
///
/// ```
/// # use browser_test::{TestFilter, TestSuite};
/// # fn suite() -> TestSuite { TestSuite::sequential() }
/// # fn checks() -> TestSuite { TestSuite::sequential() }
/// let tests = TestSuite::sequential()
///     .with_nested(suite().filter(&TestFilter::from_env()?))
///     .with_nested(checks());
/// # Ok::<(), browser_test::InvalidEnvVar>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestFilter {
    name_substrings: Vec<String>,
    tags: Vec<String>,
}

impl TestFilter {
    /// A filter selecting every test.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Also select tests whose name contains `substring`.
    ///
    /// The first substring restricts the selection to matching names, every further one widens it.
    /// An empty `substring` is ignored, as every name contains it.
    #[must_use]
    pub fn with_name_containing(mut self, substring: impl Into<String>) -> Self {
        let substring = substring.into();
        if !substring.is_empty() {
            self.name_substrings.push(substring);
        }
        self
    }

    /// Also select tests tagged exactly `tag` (see [`BrowserTest::tags`]).
    ///
    /// The first tag restricts the selection to tests with that tag, every further one widens it.
    /// Untagged tests are no longer selected then. An empty `tag` is ignored.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        let tag = tag.into();
        if !tag.is_empty() {
            self.tags.push(tag);
        }
        self
    }

    /// Read name substrings from `BROWSER_TEST_FILTER` and tags from `BROWSER_TEST_TAG`.
    ///
    /// See [`Self::with_name_substrings_from_env_var`] and [`Self::with_tags_from_env_var`] for
    /// the syntax. Unset variables select every test.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if a variable is not valid Unicode.
    pub fn from_env() -> Result<Self, InvalidEnvVar> {
        Self::new()
            .with_name_substrings_from_env_var(DEFAULT_NAME_FILTER_ENV)?
            .with_tags_from_env_var(DEFAULT_TAG_FILTER_ENV)
    }

    /// Add the comma-separated name substrings of the variable `env_var`, as
    /// [`Self::with_name_containing`] does.
    ///
    /// Whitespace around each substring is ignored, and so are empty ones: an unset or empty
    /// variable adds none. `BROWSER_TEST_FILTER=press,keyboard` selects tests whose names contain
    /// `press` or `keyboard`. The variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not valid Unicode.
    pub fn with_name_substrings_from_env_var(
        self,
        env_var: impl AsRef<str>,
    ) -> Result<Self, InvalidEnvVar> {
        Ok(env_list(env_var.as_ref())?
            .into_iter()
            .fold(self, Self::with_name_containing))
    }

    /// Add the comma-separated tags of the variable `env_var`, as [`Self::with_tag`] does.
    ///
    /// Whitespace around each tag is ignored, and so are empty ones: an unset or empty variable
    /// adds none. `BROWSER_TEST_TAG=button,focus` selects the tests tagged `button` or `focus`.
    /// The variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not valid Unicode.
    pub fn with_tags_from_env_var(self, env_var: impl AsRef<str>) -> Result<Self, InvalidEnvVar> {
        Ok(env_list(env_var.as_ref())?
            .into_iter()
            .fold(self, Self::with_tag))
    }

    /// Whether this filter selects every test.
    #[must_use]
    pub fn selects_all(&self) -> bool {
        self.name_substrings.is_empty() && self.tags.is_empty()
    }

    /// Whether this filter selects `test`. Reads only the metadata it filters by, and selects a
    /// test whose name or tags panic, so that the run reports it as failed.
    pub(crate) fn selects<Context: Sync + ?Sized>(&self, test: &dyn BrowserTest<Context>) -> bool {
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            let name_matches = self.name_substrings.is_empty() || {
                let name = test.name();
                self.name_substrings
                    .iter()
                    .any(|substring| name.contains(substring.as_str()))
            };
            name_matches
                && (self.tags.is_empty()
                    || test
                        .tags()
                        .iter()
                        .any(|tag| self.tags.iter().any(|wanted| wanted == tag)))
        }))
        .unwrap_or(true)
    }
}

/// The comma-separated values of the variable `name`, trimmed, without empty ones.
fn env_list(name: &str) -> Result<Vec<String>, InvalidEnvVar> {
    match std::env::var(name) {
        Ok(value) => Ok(value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
        Err(std::env::VarError::NotUnicode(value)) => Err(InvalidEnvVar {
            name: name.to_owned(),
            value: value.to_string_lossy().into_owned(),
            expected: "a Unicode comma-separated list",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvVarGuard;

    #[test]
    fn environment_lists_trim_and_ignore_empty_parts() {
        let names = EnvVarGuard::new(DEFAULT_NAME_FILTER_ENV);
        let tags = EnvVarGuard::new_unlocked(DEFAULT_TAG_FILTER_ENV);
        names.remove();
        tags.remove();
        assert_eq!(TestFilter::from_env().unwrap(), TestFilter::new());
        assert!(TestFilter::from_env().unwrap().selects_all());
        names.set(" , \t,");
        tags.set("");
        assert_eq!(TestFilter::from_env().unwrap(), TestFilter::new());
        names.set(" hover, ,press ");
        tags.set(" button, focus, ");
        assert_eq!(
            TestFilter::from_env().unwrap(),
            TestFilter::new()
                .with_name_containing("hover")
                .with_name_containing("press")
                .with_tag("button")
                .with_tag("focus")
        );
    }

    #[test]
    fn environment_lists_add_to_the_filter() {
        let names = EnvVarGuard::new("BROWSER_TEST_FILTER_CUSTOM");
        names.set("press");
        assert_eq!(
            TestFilter::new()
                .with_name_containing("hover")
                .with_name_substrings_from_env_var("BROWSER_TEST_FILTER_CUSTOM")
                .unwrap(),
            TestFilter::new()
                .with_name_containing("hover")
                .with_name_containing("press")
        );
    }

    #[test]
    fn empty_alternatives_are_ignored() {
        assert!(
            TestFilter::new()
                .with_name_containing("")
                .with_tag("")
                .selects_all()
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_filter_is_an_error() {
        use std::os::unix::ffi::OsStrExt;
        let names = EnvVarGuard::new("BROWSER_TEST_FILTER_INVALID");
        names.set(std::ffi::OsStr::from_bytes(&[0xff]));
        let error = TestFilter::new()
            .with_name_substrings_from_env_var("BROWSER_TEST_FILTER_INVALID")
            .unwrap_err();
        assert_eq!(error.name, "BROWSER_TEST_FILTER_INVALID");
    }
}

//! Selecting the tests of a run by name and logical group.

use crate::{BrowserTests, InvalidEnvVar};

/// Default environment variable read by [`TestFilter::from_env`] for name substrings.
pub(crate) const DEFAULT_NAME_FILTER_ENV: &str = "BROWSER_TEST_FILTER";

/// Default environment variable read by [`TestFilter::from_env`] for group names.
pub(crate) const DEFAULT_GROUP_FILTER_ENV: &str = "BROWSER_TEST_GROUP";

/// Selects tests by name substring and by exact [`TestGroup`](crate::TestGroup) name.
///
/// A test is selected if its [name](crate::BrowserTest::name) contains any of the name
/// substrings, and it belongs to a logical group with any of the group names. A filter without
/// name substrings selects tests of any name, and one without group names selects tests of any
/// group, including ungrouped tests. [`Self::new`] selects every test. Matching is case-sensitive.
///
/// Apply a filter with [`BrowserTests::filter`]. Selection is never implicit: a runner runs every
/// test it is given, whatever the environment says.
///
/// # Examples
///
/// Select the tests of the groups `button` and `focus` whose names contain `keyboard`:
///
/// ```
/// use browser_test::TestFilter;
///
/// let filter = TestFilter::new()
///     .with_group("button")
///     .with_group("focus")
///     .with_name_containing("keyboard");
/// ```
///
/// Let whoever runs the tests select them through `BROWSER_TEST_FILTER` and `BROWSER_TEST_GROUP`,
/// and add checks of the whole run after filtering, so that they always run:
///
/// ```
/// # use browser_test::{BrowserTests, Parallelism, TestFilter};
/// # fn suite() -> BrowserTests { BrowserTests::sequential() }
/// # fn checks() -> BrowserTests { BrowserTests::sequential() }
/// let tests = BrowserTests::sequential()
///     .with_group(suite().filter(&TestFilter::from_env()?))
///     .with_group(checks());
/// # Ok::<(), browser_test::InvalidEnvVar>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestFilter {
    name_substrings: Vec<String>,
    groups: Vec<String>,
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

    /// Also select tests of the [`TestGroup`](crate::TestGroup) named exactly `group`.
    ///
    /// The first group restricts the selection to tests of matching groups, every further one
    /// widens it. Ungrouped tests are no longer selected then. An empty `group` is ignored.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        let group = group.into();
        if !group.is_empty() {
            self.groups.push(group);
        }
        self
    }

    /// Read name substrings from `BROWSER_TEST_FILTER` and group names from `BROWSER_TEST_GROUP`.
    ///
    /// See [`Self::with_name_substrings_from_env_var`] and [`Self::with_groups_from_env_var`] for
    /// the syntax. Unset variables select every test.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if a variable is not valid Unicode.
    pub fn from_env() -> Result<Self, InvalidEnvVar> {
        Self::new()
            .with_name_substrings_from_env_var(DEFAULT_NAME_FILTER_ENV)?
            .with_groups_from_env_var(DEFAULT_GROUP_FILTER_ENV)
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

    /// Add the comma-separated group names of the variable `env_var`, as [`Self::with_group`]
    /// does.
    ///
    /// Whitespace around each name is ignored, and so are empty ones: an unset or empty variable
    /// adds none. `BROWSER_TEST_GROUP=button,focus` selects the tests of these two groups. The
    /// variable is read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not valid Unicode.
    pub fn with_groups_from_env_var(self, env_var: impl AsRef<str>) -> Result<Self, InvalidEnvVar> {
        Ok(env_list(env_var.as_ref())?
            .into_iter()
            .fold(self, Self::with_group))
    }

    /// Whether this filter selects every test.
    #[must_use]
    pub fn selects_all(&self) -> bool {
        self.name_substrings.is_empty() && self.groups.is_empty()
    }

    pub(crate) fn apply<Context: Sync + ?Sized, TestError: ?Sized>(
        &self,
        mut tests: BrowserTests<Context, TestError>,
    ) -> BrowserTests<Context, TestError> {
        if !self.groups.is_empty() {
            tests = tests.filter_groups(|name| self.groups.iter().any(|group| group == name));
        }
        if !self.name_substrings.is_empty() {
            tests = tests.filter_tests(|name| {
                self.name_substrings
                    .iter()
                    .any(|substring| name.contains(substring.as_str()))
            });
        }
        tests
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
        let groups = EnvVarGuard::new_unlocked(DEFAULT_GROUP_FILTER_ENV);
        names.remove();
        groups.remove();
        assert_eq!(TestFilter::from_env().unwrap(), TestFilter::new());
        assert!(TestFilter::from_env().unwrap().selects_all());
        names.set(" , \t,");
        groups.set("");
        assert_eq!(TestFilter::from_env().unwrap(), TestFilter::new());
        names.set(" hover, ,press ");
        groups.set(" button, focus, ");
        assert_eq!(
            TestFilter::from_env().unwrap(),
            TestFilter::new()
                .with_name_containing("hover")
                .with_name_containing("press")
                .with_group("button")
                .with_group("focus")
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
                .with_group("")
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

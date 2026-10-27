use std::{env, num::NonZeroUsize};

/// An environment variable whose value cannot be interpreted, returned by the `from_env` and
/// `from_env_var` constructors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "environment variable {name} is set to {value:?}, expected {expected}{}",
    details.as_ref().map(|details| format!(": {details}")).unwrap_or_default()
)]
#[non_exhaustive]
pub struct InvalidEnvVar {
    /// The variable's name.
    pub name: String,

    /// The variable's value, lossily converted to UTF-8.
    pub value: String,

    /// What the value should have been.
    pub expected: &'static str,

    /// What is wrong with the value, if more is known than that it isn't what was expected, e.g.
    /// the problem a regular expression has.
    pub details: Option<String>,
}

/// Read `name`'s value, trimmed. `None` if the variable is unset or empty.
fn env_value(name: &str) -> Option<String> {
    let value = env::var_os(name)?;
    let value = value.to_string_lossy().trim().to_owned();
    (!value.is_empty()).then_some(value)
}

/// Read `name`'s value, trimmed, failing if it is not valid Unicode instead of converting it
/// lossily. `None` if the variable is unset or empty.
pub(crate) fn env_unicode_value(
    name: &str,
    expected: &'static str,
) -> Result<Option<String>, InvalidEnvVar> {
    match env::var(name) {
        Ok(value) => {
            let value = value.trim();
            Ok((!value.is_empty()).then(|| value.to_owned()))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(value)) => Err(InvalidEnvVar {
            name: name.to_owned(),
            value: value.to_string_lossy().into_owned(),
            expected,
            details: Some("the value is not valid Unicode".to_owned()),
        }),
    }
}

/// Read a boolean flag. `None` if the variable is unset or empty.
///
/// `1`, `true`, `yes`, `on`, and `enabled` enable the flag, `0`, `false`, `no`, `off`, and
/// `disabled` disable it (ignoring case).
pub(crate) fn env_flag(name: &str) -> Result<Option<bool>, InvalidEnvVar> {
    let Some(value) = env_value(name) else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" => Ok(Some(true)),
        "0" | "false" | "no" | "off" | "disabled" => Ok(Some(false)),
        _ => Err(InvalidEnvVar {
            name: name.to_owned(),
            value,
            expected: "one of 1, true, yes, on, enabled, 0, false, no, off, or disabled",
            details: None,
        }),
    }
}

/// Read a positive number. `None` if the variable is unset or empty.
pub(crate) fn env_positive_number(name: &str) -> Result<Option<NonZeroUsize>, InvalidEnvVar> {
    let Some(value) = env_value(name) else {
        return Ok(None);
    };
    match value.parse::<NonZeroUsize>() {
        Ok(number) => Ok(Some(number)),
        Err(_) => Err(InvalidEnvVar {
            name: name.to_owned(),
            value,
            expected: "a positive number",
            details: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;
    use crate::test_support::EnvVarGuard;

    const TEST_VAR: &str = "BROWSER_TEST_ENV_TEST";

    #[test]
    fn unset_and_empty_variables_have_no_value() {
        let env = EnvVarGuard::new(TEST_VAR);
        env.remove();
        assert_that!(env_flag(TEST_VAR)).is_equal_to(Ok(None));
        assert_that!(env_positive_number(TEST_VAR)).is_equal_to(Ok(None));

        env.set(" ");
        assert_that!(env_flag(TEST_VAR)).is_equal_to(Ok(None));
        assert_that!(env_positive_number(TEST_VAR)).is_equal_to(Ok(None));
    }

    #[test]
    fn flags_accept_conventional_values_ignoring_case() {
        let env = EnvVarGuard::new(TEST_VAR);
        for value in ["1", "true", "YES", " on ", "Enabled"] {
            env.set(value);
            assert_that!(env_flag(TEST_VAR))
                .with_detail_message(format!("Testing: '{value}'"))
                .is_equal_to(Ok(Some(true)));
        }
        for value in ["0", "FALSE", "no", "off", "disabled"] {
            env.set(value);
            assert_that!(env_flag(TEST_VAR))
                .with_detail_message(format!("Testing: '{value}'"))
                .is_equal_to(Ok(Some(false)));
        }
    }

    #[test]
    fn invalid_values_are_errors() {
        let env = EnvVarGuard::new(TEST_VAR);
        env.set("ture");
        let error = env_flag(TEST_VAR).expect_err("an unknown flag value is invalid");
        assert_that!(error.to_string()).contains("BROWSER_TEST_ENV_TEST is set to \"ture\"");

        env.set("four");
        assert_that!(env_positive_number(TEST_VAR)).is_err();
        env.set("0");
        assert_that!(env_positive_number(TEST_VAR)).is_err();
        env.set(" 4 ");
        assert_that!(env_positive_number(TEST_VAR)).is_equal_to(Ok(NonZeroUsize::new(4)));
    }
}

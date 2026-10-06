use std::env;

/// An environment variable whose value cannot be interpreted, returned by the `from_env` and
/// `from_env_var` constructors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("environment variable {name} is set to {value:?}, expected {expected}")]
#[non_exhaustive]
pub struct InvalidEnvVar {
    /// The variable's name.
    pub name: String,

    /// The variable's value, lossily converted to UTF-8.
    pub value: String,

    /// What the value should have been.
    pub expected: &'static str,
}

/// Read `name`'s value, trimmed. `None` if the variable is unset or empty.
fn env_value(name: &str) -> Option<String> {
    let value = env::var_os(name)?;
    let value = value.to_string_lossy().trim().to_owned();
    (!value.is_empty()).then_some(value)
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
        }),
    }
}

/// Read a non-negative number. `None` if the variable is unset or empty.
pub(crate) fn env_number(name: &str) -> Result<Option<usize>, InvalidEnvVar> {
    let Some(value) = env_value(name) else {
        return Ok(None);
    };
    match value.parse::<usize>() {
        Ok(number) => Ok(Some(number)),
        Err(_) => Err(InvalidEnvVar {
            name: name.to_owned(),
            value,
            expected: "a non-negative number",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvVarGuard;
    use assertr::prelude::*;

    const TEST_VAR: &str = "BROWSER_TEST_ENV_TEST";

    #[test]
    fn unset_and_empty_variables_have_no_value() {
        let env = EnvVarGuard::new(TEST_VAR);
        env.remove();
        assert_that!(env_flag(TEST_VAR)).is_equal_to(Ok(None));
        assert_that!(env_number(TEST_VAR)).is_equal_to(Ok(None));

        env.set(" ");
        assert_that!(env_flag(TEST_VAR)).is_equal_to(Ok(None));
        assert_that!(env_number(TEST_VAR)).is_equal_to(Ok(None));
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
        assert_that!(env_number(TEST_VAR).is_err()).is_true();
        env.set(" 4 ");
        assert_that!(env_number(TEST_VAR)).is_equal_to(Ok(Some(4)));
    }
}

use std::env;

/// Read a conventional boolean flag from the environment.
///
/// The variable is considered **disabled** if it is:\
/// ->  unset, empty, `0`, `false`, `no`, `off` or `disabled`.
///
/// The variable is considered **enabled** if it is:\
/// ->  set, non-empty, `1`, `true`, `yes`, `on` or `enabled`.
///
/// The input is converted `to_ascii_lowercase` so the checks are case-insensitive.
#[must_use]
pub(crate) fn env_flag_enabled(env_var: impl AsRef<str>) -> bool {
    let Some(value) = env::var_os(env_var.as_ref()) else {
        return false;
    };
    let value = value.to_string_lossy();
    let normalized = value.trim().to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "" | "0" | "false" | "no" | "off" | "disabled"
    ) {
        return false;
    }
    if matches!(normalized.as_str(), "1" | "true" | "yes" | "on" | "enabled") {
        return true;
    }
    false
}

/// Read an optional boolean flag from the environment.
///
/// Returns `None` if the variable is unset or empty (after trimming), so that callers can fall back
/// to their default. Otherwise, interprets the value like [`env_flag_enabled`].
#[must_use]
pub(crate) fn env_flag_value(env_var: impl AsRef<str>) -> Option<bool> {
    let value = env::var_os(env_var.as_ref())?;
    if value.to_string_lossy().trim().is_empty() {
        return None;
    }
    Some(env_flag_enabled(env_var))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvVarGuard;
    use assertr::prelude::*;

    const ENV_FLAG_TEST_VAR: &str = "BROWSER_TEST_ENV_FLAG_ENABLED_TEST";

    mod env_flag_enabled {
        use super::*;

        #[test]
        fn treats_unset_as_disabled() {
            let env = EnvVarGuard::new(ENV_FLAG_TEST_VAR);
            env.remove();

            assert_that!(env_flag_enabled(ENV_FLAG_TEST_VAR)).is_false();
        }

        #[test]
        fn treats_disabled_values_as_disabled() {
            let env = EnvVarGuard::new(ENV_FLAG_TEST_VAR);

            for value in [
                "", " ", "0", "false", "FALSE", " no ", "off", "Off", "disabled",
            ] {
                env.set(value);
                assert_that!(env_flag_enabled(ENV_FLAG_TEST_VAR))
                    .with_detail_message(format!("Testing: '{value}'"))
                    .is_false();
            }
        }

        #[test]
        fn treats_truthy_values_as_enabled() {
            let env = EnvVarGuard::new(ENV_FLAG_TEST_VAR);

            for value in ["1", "true", "yes", "YES", "on", "ON", "enabled"] {
                env.set(value);
                assert_that!(env_flag_enabled(ENV_FLAG_TEST_VAR))
                    .with_detail_message(format!("Testing: '{value}'"))
                    .is_true();
            }
        }

        #[test]
        fn treats_other_values_as_disabled() {
            let env = EnvVarGuard::new(ENV_FLAG_TEST_VAR);

            for value in ["2", "foo"] {
                env.set(value);
                assert_that!(env_flag_enabled(ENV_FLAG_TEST_VAR))
                    .with_detail_message(format!("Testing: '{value}'"))
                    .is_false();
            }
        }
    }

    mod env_flag_value {
        use super::*;

        #[test]
        fn is_none_when_unset_or_empty() {
            let env = EnvVarGuard::new(ENV_FLAG_TEST_VAR);
            env.remove();
            assert_that!(env_flag_value(ENV_FLAG_TEST_VAR)).is_none();

            env.set(" ");
            assert_that!(env_flag_value(ENV_FLAG_TEST_VAR)).is_none();
        }

        #[test]
        fn interprets_set_values() {
            let env = EnvVarGuard::new(ENV_FLAG_TEST_VAR);
            env.set("off");
            assert_that!(env_flag_value(ENV_FLAG_TEST_VAR)).is_equal_to(Some(false));

            env.set("yes");
            assert_that!(env_flag_value(ENV_FLAG_TEST_VAR)).is_equal_to(Some(true));
        }
    }
}

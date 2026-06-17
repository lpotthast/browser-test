use std::borrow::Cow;
use std::io::ErrorKind;

use rootcause::Report;
use rootcause::prelude::ResultExt;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::BrowserTestError;
use crate::env::env_flag_enabled;

pub(crate) const DEFAULT_PAUSE_ENV: &str = "BROWSER_TEST_PAUSE";

/// Configuration for the manual pause before browser tests execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseConfig {
    activation: PauseActivation,
    message: Cow<'static, str>,
    prompt: Cow<'static, str>,
}

/// Resolved configuration for the manual pause before browser tests execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPauseConfig {
    enabled: bool,
    message: Cow<'static, str>,
    prompt: Cow<'static, str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
enum PauseActivation {
    #[default]
    Disabled,
    Enabled,
    FromEnvVar(String),
    FromEnv,
}

impl Default for PauseConfig {
    fn default() -> Self {
        Self {
            activation: PauseActivation::Disabled,
            message: "Browser test execution is paused.".into(),
            prompt: "Continue with tests? [y/N] ".into(),
        }
    }
}

impl PauseConfig {
    /// Build a disabled pause config.
    #[must_use]
    pub fn disabled() -> Self {
        Self::enabled(false)
    }

    /// Build a pause config from `BROWSER_TEST_PAUSE`.
    ///
    /// The variable is considered enabled unless it is unset, empty, `0`, `false`, `no`, or `off`.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            activation: PauseActivation::FromEnv,
            ..Self::default()
        }
    }

    /// Build a pause config from an environment variable.
    ///
    /// The variable is considered enabled unless it is unset, empty, `0`, `false`, `no`, or `off`.
    #[must_use]
    pub fn from_env_var(env_var: impl AsRef<str>) -> Self {
        Self {
            activation: PauseActivation::FromEnvVar(env_var.as_ref().to_owned()),
            ..Self::default()
        }
    }

    /// Build an enabled or disabled pause config directly.
    #[must_use]
    pub fn enabled(enabled: bool) -> Self {
        Self {
            activation: PauseActivation::from_enabled(enabled),
            ..Self::default()
        }
    }

    /// Set the message printed before the prompt.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<Cow<'static, str>>) -> Self {
        self.message = message.into();
        self
    }

    /// Set the interactive prompt.
    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<Cow<'static, str>>) -> Self {
        self.prompt = prompt.into();
        self
    }

    /// Resolve this config to an immediate pause config.
    ///
    /// Environment-backed configs read their environment variable when this method is called.
    #[must_use]
    pub fn resolve(&self) -> ResolvedPauseConfig {
        ResolvedPauseConfig {
            enabled: self.activation.resolve(),
            message: self.message.clone(),
            prompt: self.prompt.clone(),
        }
    }

    /// Resolve this config and report whether the pause is enabled.
    ///
    /// Environment-backed configs read their environment variable when this method is called.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.resolve().is_enabled()
    }
}

impl ResolvedPauseConfig {
    /// Build a disabled resolved pause config.
    #[must_use]
    pub fn disabled() -> Self {
        Self::enabled(false)
    }

    /// Build an enabled or disabled resolved pause config directly.
    #[must_use]
    pub fn enabled(enabled: bool) -> Self {
        Self {
            enabled,
            message: "Browser test execution is paused.".into(),
            prompt: "Continue with tests? [y/N] ".into(),
        }
    }

    /// Set the message printed before the prompt.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<Cow<'static, str>>) -> Self {
        self.message = message.into();
        self
    }

    /// Set the interactive prompt.
    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<Cow<'static, str>>) -> Self {
        self.prompt = prompt.into();
        self
    }

    /// Whether the pause is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl PauseActivation {
    const fn from_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }

    fn resolve(&self) -> bool {
        match self {
            Self::Disabled => false,
            Self::Enabled => true,
            Self::FromEnvVar(env_var) => env_flag_enabled(env_var),
            Self::FromEnv => env_flag_enabled(DEFAULT_PAUSE_ENV),
        }
    }
}

impl From<ResolvedPauseConfig> for PauseConfig {
    fn from(config: ResolvedPauseConfig) -> Self {
        Self {
            activation: PauseActivation::from_enabled(config.enabled),
            message: config.message,
            prompt: config.prompt,
        }
    }
}

/// The user's choice after a pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PauseDecision {
    /// Continue with the browser tests.
    Continue,

    /// Abort before running browser tests.
    Abort,
}

pub(crate) async fn pause_if_requested(
    config: ResolvedPauseConfig,
    hint: Option<&str>,
) -> Result<PauseDecision, Report<BrowserTestError>> {
    if !config.enabled {
        return Ok(PauseDecision::Continue);
    }
    pause(config, hint).await
}

async fn pause(
    config: ResolvedPauseConfig,
    hint: Option<&str>,
) -> Result<PauseDecision, Report<BrowserTestError>> {
    let mut stdin = io::BufReader::new(io::stdin());
    let mut stdout = io::stdout();

    pause_with_io(config, hint, &mut stdin, &mut stdout).await
}

async fn pause_with_io<R, W>(
    config: ResolvedPauseConfig,
    hint: Option<&str>,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<PauseDecision, Report<BrowserTestError>>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    stdout
        .write_all(config.message.as_bytes())
        .await
        .context(BrowserTestError::FlushPausePrompt)?;
    stdout
        .write_all(b"\n")
        .await
        .context(BrowserTestError::FlushPausePrompt)?;
    tracing::info!("{}", config.message);

    if let Some(hint) = hint.filter(|hint| !hint.is_empty()) {
        stdout
            .write_all(hint.as_bytes())
            .await
            .context(BrowserTestError::FlushPausePrompt)?;
        stdout
            .write_all(b"\n")
            .await
            .context(BrowserTestError::FlushPausePrompt)?;
        tracing::info!("{hint}");
    }

    let mut buf = String::new();
    loop {
        stdout
            .write_all(config.prompt.as_bytes())
            .await
            .context(BrowserTestError::FlushPausePrompt)?;
        stdout
            .flush()
            .await
            .context(BrowserTestError::FlushPausePrompt)?;

        buf.clear();
        let bytes_read = stdin
            .read_line(&mut buf)
            .await
            .context(BrowserTestError::ReadPauseResponse)?;
        if bytes_read == 0 {
            return Err(Err::<(), _>(io::Error::new(
                ErrorKind::UnexpectedEof,
                "stdin reached EOF while waiting for pause response",
            ))
            .context(BrowserTestError::ReadPauseResponse)
            .expect_err("synthetic EOF error should always be an error"));
        }

        match buf.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" | "c" | "continue" => return Ok(PauseDecision::Continue),
            "n" | "no" | "q" | "quit" | "" => return Ok(PauseDecision::Abort),
            _ => {
                stdout
                    .write_all(b"Enter 'y' to continue or 'n' to abort.\n")
                    .await
                    .context(BrowserTestError::FlushPausePrompt)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvVarGuard;
    use assertr::prelude::*;
    use tokio::io::BufReader;

    mod pause_config {
        use super::*;

        #[test]
        fn default_is_disabled() {
            assert_that!(PauseConfig::default().is_enabled()).is_false();
        }

        #[test]
        fn resolved_pause_reports_enabled_state() {
            assert_that!(ResolvedPauseConfig::enabled(true).is_enabled()).is_true();
            assert_that!(ResolvedPauseConfig::enabled(false).is_enabled()).is_false();
            assert_that!(ResolvedPauseConfig::disabled().is_enabled()).is_false();
        }

        mod from_env {
            use super::*;

            #[test]
            fn from_env_treats_unset_as_disabled() {
                let env = EnvVarGuard::new("BROWSER_TEST_PAUSE_CONFIG_TEST");
                env.remove();

                assert_that!(
                    PauseConfig::from_env_var("BROWSER_TEST_PAUSE_CONFIG_TEST")
                        .resolve()
                        .is_enabled()
                )
                .is_false();
            }

            #[test]
            fn from_env_reads_default_pause_var() {
                let env = EnvVarGuard::new(DEFAULT_PAUSE_ENV);
                env.set("yes");
                assert_that!(PauseConfig::from_env().resolve().is_enabled()).is_true();
                env.set("no");
                assert_that!(PauseConfig::from_env().resolve().is_enabled()).is_false();
            }

            #[test]
            fn is_enabled_resolves_env_backed_config() {
                let env = EnvVarGuard::new(DEFAULT_PAUSE_ENV);
                env.set("yes");
                assert_that!(PauseConfig::from_env().is_enabled()).is_true();
            }
        }
    }

    mod pause {
        use super::*;

        #[tokio::test]
        async fn treats_stdin_eof_as_read_error() {
            let mut stdin = BufReader::new(&b""[..]);
            let mut stdout = Vec::new();

            let err = pause_with_io(
                ResolvedPauseConfig::enabled(true),
                None,
                &mut stdin,
                &mut stdout,
            )
            .await
            .expect_err("stdin EOF should fail instead of aborting");

            assert_that!(err.to_string()).contains(BrowserTestError::ReadPauseResponse.to_string());
            assert_that!(format!("{err:?}"))
                .contains("stdin reached EOF while waiting for pause response");
        }

        #[tokio::test]
        async fn treats_empty_line_as_abort() {
            let mut stdin = BufReader::new(&b"\n"[..]);
            let mut stdout = Vec::new();

            let decision = pause_with_io(
                ResolvedPauseConfig::enabled(true),
                None,
                &mut stdin,
                &mut stdout,
            )
            .await
            .expect("empty line should remain an explicit abort response");

            assert_that!(decision).is_equal_to(PauseDecision::Abort);
        }

        #[tokio::test]
        async fn treats_y_as_continue() {
            let mut stdin = BufReader::new(&b"y\n"[..]);
            let mut stdout = Vec::new();

            let decision = pause_with_io(
                ResolvedPauseConfig::enabled(true),
                None,
                &mut stdin,
                &mut stdout,
            )
            .await
            .expect("positive response should continue");

            assert_that!(decision).is_equal_to(PauseDecision::Continue);
        }
    }
}

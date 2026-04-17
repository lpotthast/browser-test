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
    enabled: bool,
    message: Cow<'static, str>,
    prompt: Cow<'static, str>,
}

impl Default for PauseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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
        Self::from_env_var(DEFAULT_PAUSE_ENV)
    }

    /// Build a pause config from an environment variable.
    ///
    /// The variable is considered enabled unless it is unset, empty, `0`, `false`, `no`, or `off`.
    #[must_use]
    pub fn from_env_var(env_var: impl AsRef<str>) -> Self {
        Self {
            enabled: env_flag_enabled(env_var),
            ..Self::default()
        }
    }

    /// Build an enabled or disabled pause config directly.
    #[must_use]
    pub fn enabled(enabled: bool) -> Self {
        Self {
            enabled,
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

    /// Whether the pause is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
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
    config: PauseConfig,
    hint: Option<&str>,
) -> Result<PauseDecision, Report<BrowserTestError>> {
    if !config.enabled {
        return Ok(PauseDecision::Continue);
    }
    pause(config, hint).await
}

async fn pause(
    config: PauseConfig,
    hint: Option<&str>,
) -> Result<PauseDecision, Report<BrowserTestError>> {
    let mut stdin = io::BufReader::new(io::stdin());
    let mut stdout = io::stdout();

    pause_with_io(config, hint, &mut stdin, &mut stdout).await
}

async fn pause_with_io<R, W>(
    config: PauseConfig,
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

        mod from_env {
            use super::*;

            #[test]
            fn from_env_treats_unset_as_disabled() {
                let env = EnvVarGuard::new("BROWSER_TEST_PAUSE_CONFIG_TEST");
                env.remove();

                assert_that!(
                    PauseConfig::from_env_var("BROWSER_TEST_PAUSE_CONFIG_TEST").is_enabled()
                )
                .is_false();
            }

            #[test]
            fn from_env_reads_default_pause_var() {
                let env = EnvVarGuard::new(DEFAULT_PAUSE_ENV);
                env.set("yes");
                assert_that!(PauseConfig::from_env().is_enabled()).is_true();
                env.set("no");
                assert_that!(PauseConfig::from_env().is_enabled()).is_false();
            }
        }
    }

    mod pause {
        use super::*;

        #[tokio::test]
        async fn treats_stdin_eof_as_read_error() {
            let mut stdin = BufReader::new(&b""[..]);
            let mut stdout = Vec::new();

            let err = pause_with_io(PauseConfig::enabled(true), None, &mut stdin, &mut stdout)
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

            let decision = pause_with_io(PauseConfig::enabled(true), None, &mut stdin, &mut stdout)
                .await
                .expect("empty line should remain an explicit abort response");

            assert_that!(decision).is_equal_to(PauseDecision::Abort);
        }

        #[tokio::test]
        async fn treats_y_as_continue() {
            let mut stdin = BufReader::new(&b"y\n"[..]);
            let mut stdout = Vec::new();

            let decision = pause_with_io(PauseConfig::enabled(true), None, &mut stdin, &mut stdout)
                .await
                .expect("positive response should continue");

            assert_that!(decision).is_equal_to(PauseDecision::Continue);
        }
    }
}

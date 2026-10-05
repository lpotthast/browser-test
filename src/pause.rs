use std::borrow::Cow;
use std::fmt::Display;
use std::io::ErrorKind;

use rootcause::Report;
use rootcause::prelude::ResultExt;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::BrowserTestError;
use crate::env::{InvalidEnvVar, env_flag};

pub(crate) const DEFAULT_PAUSE_ENV: &str = "BROWSER_TEST_PAUSE";

/// The manual pause before browser tests run, e.g. to inspect the app under test or attach a
/// debugger before tests start.
///
/// Disabled by default. When enabled, the runner prints the message, an optional hint, and the
/// prompt, and waits for an answer on stdin before starting the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pause {
    enabled: bool,
    message: Cow<'static, str>,
    prompt: Cow<'static, str>,
    hint: Option<String>,
}

impl Default for Pause {
    fn default() -> Self {
        Self::disabled()
    }
}

impl Pause {
    /// Do not pause.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            message: "Browser test execution is paused.".into(),
            prompt: "Continue with tests? [y/N] ".into(),
            hint: None,
        }
    }

    /// Pause before running browser tests.
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::disabled()
        }
    }

    /// Read whether to pause from `BROWSER_TEST_PAUSE`.
    ///
    /// See [`Self::from_env_var`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a boolean flag.
    pub fn from_env() -> Result<Option<Self>, InvalidEnvVar> {
        Self::from_env_var(DEFAULT_PAUSE_ENV)
    }

    /// Read whether to pause from the boolean flag `env_var`.
    ///
    /// Returns `None` if the variable is unset or empty, so the caller picks the default:
    /// `Pause::from_env()?.unwrap_or_default()`. `1`, `true`, `yes`, `on`, and `enabled` enable the
    /// flag; `0`, `false`, `no`, `off`, and `disabled` disable it (ignoring case). The variable is
    /// read when this function is called.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEnvVar`] if the variable is not a boolean flag.
    pub fn from_env_var(env_var: impl AsRef<str>) -> Result<Option<Self>, InvalidEnvVar> {
        Ok(env_flag(env_var.as_ref())?.map(|enabled| {
            if enabled {
                Self::enabled()
            } else {
                Self::disabled()
            }
        }))
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

    /// Set extra context printed below the message, e.g. the URL of the app under test.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Display) -> Self {
        self.hint = Some(hint.to_string());
        self
    }

    /// Whether the runner pauses before running browser tests.
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
    config: &Pause,
) -> Result<PauseDecision, Report<BrowserTestError>> {
    if !config.enabled {
        return Ok(PauseDecision::Continue);
    }
    pause(config).await
}

async fn pause(config: &Pause) -> Result<PauseDecision, Report<BrowserTestError>> {
    let mut stdin = io::BufReader::new(io::stdin());
    let mut stdout = io::stdout();
    pause_with_io(config, &mut stdin, &mut stdout).await
}

async fn pause_with_io<R, W>(
    config: &Pause,
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

    if let Some(hint) = config.hint.as_deref().filter(|hint| !hint.is_empty()) {
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
            assert_that!(Pause::default().is_enabled()).is_false();
            assert_that!(Pause::enabled().is_enabled()).is_true();
        }

        #[test]
        fn from_env_var_treats_unset_as_disabled() {
            let env = EnvVarGuard::new("BROWSER_TEST_PAUSE_CONFIG_TEST");
            env.remove();

            assert_that!(Pause::from_env_var("BROWSER_TEST_PAUSE_CONFIG_TEST"))
                .is_equal_to(Ok(None));
        }

        #[test]
        fn from_env_reads_default_pause_var() {
            let env = EnvVarGuard::new(DEFAULT_PAUSE_ENV);
            env.set("yes");
            assert_that!(Pause::from_env()).is_equal_to(Ok(Some(Pause::enabled())));
            env.set("no");
            assert_that!(Pause::from_env()).is_equal_to(Ok(Some(Pause::disabled())));
        }
    }

    mod pause {
        use super::*;

        #[tokio::test]
        async fn treats_stdin_eof_as_read_error() {
            let mut stdin = BufReader::new(&b""[..]);
            let mut stdout = Vec::new();

            let err = pause_with_io(&Pause::enabled(), &mut stdin, &mut stdout)
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

            let decision = pause_with_io(&Pause::enabled(), &mut stdin, &mut stdout)
                .await
                .expect("empty line should remain an explicit abort response");

            assert_that!(decision).is_equal_to(PauseDecision::Abort);
        }

        #[tokio::test]
        async fn prints_message_hint_and_prompt() {
            let mut stdin = BufReader::new(&b"y\n"[..]);
            let mut stdout = Vec::new();
            let config = Pause::enabled()
                .with_message("Paused.")
                .with_hint("App at http://127.0.0.1:3000")
                .with_prompt("Go? ");

            pause_with_io(&config, &mut stdin, &mut stdout)
                .await
                .expect("positive response should continue");

            assert_that!(String::from_utf8(stdout).expect("output is UTF-8"))
                .is_equal_to("Paused.\nApp at http://127.0.0.1:3000\nGo? ");
        }

        #[tokio::test]
        async fn treats_y_as_continue() {
            let mut stdin = BufReader::new(&b"y\n"[..]);
            let mut stdout = Vec::new();

            let decision = pause_with_io(&Pause::enabled(), &mut stdin, &mut stdout)
                .await
                .expect("positive response should continue");

            assert_that!(decision).is_equal_to(PauseDecision::Continue);
        }
    }
}

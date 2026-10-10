use std::{
    borrow::Cow,
    fmt::Display,
    io::{self, ErrorKind},
    thread,
};

use rootcause::{Report, prelude::ResultExt};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::oneshot,
};

use crate::{
    BrowserTestError,
    env::{InvalidEnvVar, env_flag},
};

pub(crate) const DEFAULT_PAUSE_ENV: &str = "BROWSER_TEST_PAUSE";

/// The manual pause before browser tests run, e.g. to inspect the app under test or attach a
/// debugger before tests start.
///
/// Disabled by default. When enabled, the runner prints the message, an optional hint, and the
/// prompt, and waits for an answer on stdin before starting the browser. `y`, `yes`, `c`, or
/// `continue` start the tests. `n`, `no`, `q`, `quit`, or an empty answer end the run
/// successfully without starting the browser.
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
    /// flag, `0`, `false`, `no`, `off`, and `disabled` disable it (ignoring case). The variable is
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
    pause_with_io(config, read_stdin_line, &mut tokio::io::stdout()).await
}

/// Read a line from stdin on a thread of its own. `None` at EOF.
///
/// Not through Tokio's `stdin`, which reads on the runtime's blocking pool: a read cannot be
/// cancelled, so a cancelled pause (Ctrl-C at the prompt) would keep the runtime from shutting
/// down until the user presses Enter. A thread of its own is left behind instead.
async fn read_stdin_line() -> io::Result<Option<String>> {
    let (line_sender, line) = oneshot::channel();
    thread::Builder::new()
        .name("browser-test-pause".into())
        .spawn(move || {
            let mut line = String::new();
            let read = io::stdin()
                .read_line(&mut line)
                .map(|bytes_read| (bytes_read > 0).then_some(line));
            let _ = line_sender.send(read);
        })?;
    line.await
        .unwrap_or_else(|_| Err(io::Error::other("the stdin reader thread panicked")))
}

/// Print the pause to `stdout` and ask until `read_line` (`None` at EOF) gives an answer.
async fn pause_with_io<ReadLine, W>(
    config: &Pause,
    mut read_line: impl FnMut() -> ReadLine,
    stdout: &mut W,
) -> Result<PauseDecision, Report<BrowserTestError>>
where
    ReadLine: Future<Output = io::Result<Option<String>>>,
    W: AsyncWrite + Unpin,
{
    print(stdout, &format!("{}\n", config.message)).await?;
    tracing::info!("{}", config.message);

    if let Some(hint) = config.hint.as_deref().filter(|hint| !hint.is_empty()) {
        print(stdout, &format!("{hint}\n")).await?;
        tracing::info!("{hint}");
    }

    loop {
        print(stdout, &config.prompt).await?;
        let Some(answer) = read_line()
            .await
            .context(BrowserTestError::ReadPauseResponse)?
        else {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "stdin reached EOF while waiting for pause response",
            ))
            .context(BrowserTestError::ReadPauseResponse);
        };

        match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" | "c" | "continue" => return Ok(PauseDecision::Continue),
            "n" | "no" | "q" | "quit" | "" => return Ok(PauseDecision::Abort),
            _ => print(stdout, "Enter 'y' to continue or 'n' to abort.\n").await?,
        }
    }
}

/// Write `text` to `stdout` and flush it, as the prompt waits for input on its line.
async fn print<W: AsyncWrite + Unpin>(
    stdout: &mut W,
    text: &str,
) -> Result<(), Report<BrowserTestError>> {
    stdout
        .write_all(text.as_bytes())
        .await
        .context(BrowserTestError::WritePausePrompt)?;
    stdout
        .flush()
        .await
        .context(BrowserTestError::WritePausePrompt)
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;
    use crate::test_support::EnvVarGuard;

    /// Reads `lines`, one per call, then EOF.
    fn answers(lines: &[&str]) -> impl FnMut() -> std::future::Ready<io::Result<Option<String>>> {
        let mut lines = lines
            .iter()
            .map(|line| (*line).to_owned())
            .collect::<Vec<_>>()
            .into_iter();
        move || std::future::ready(Ok(lines.next()))
    }

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
            let stdin = answers(&[]);
            let mut stdout = Vec::new();

            let err = pause_with_io(&Pause::enabled(), stdin, &mut stdout)
                .await
                .expect_err("stdin EOF should fail instead of aborting");

            assert_that!(err.to_string()).contains(BrowserTestError::ReadPauseResponse.to_string());
            assert_that!(format!("{err:?}"))
                .contains("stdin reached EOF while waiting for pause response");
        }

        #[tokio::test]
        async fn treats_empty_line_as_abort() {
            let stdin = answers(&["\n"]);
            let mut stdout = Vec::new();

            let decision = pause_with_io(&Pause::enabled(), stdin, &mut stdout)
                .await
                .expect("empty line should remain an explicit abort response");

            assert_that!(decision).is_equal_to(PauseDecision::Abort);
        }

        #[tokio::test]
        async fn prints_message_hint_and_prompt() {
            let stdin = answers(&["y\n"]);
            let mut stdout = Vec::new();
            let config = Pause::enabled()
                .with_message("Paused.")
                .with_hint("App at http://127.0.0.1:3000")
                .with_prompt("Go? ");

            pause_with_io(&config, stdin, &mut stdout)
                .await
                .expect("positive response should continue");

            assert_that!(String::from_utf8(stdout).expect("output is UTF-8"))
                .is_equal_to("Paused.\nApp at http://127.0.0.1:3000\nGo? ");
        }

        #[tokio::test]
        async fn treats_y_as_continue() {
            let stdin = answers(&["y\n"]);
            let mut stdout = Vec::new();

            let decision = pause_with_io(&Pause::enabled(), stdin, &mut stdout)
                .await
                .expect("positive response should continue");

            assert_that!(decision).is_equal_to(PauseDecision::Continue);
        }

        #[tokio::test]
        async fn asks_again_after_an_unknown_answer() {
            let stdin = answers(&["maybe\n", "n\n"]);
            let mut stdout = Vec::new();
            let config = Pause::enabled().with_message("Paused.").with_prompt("Go? ");

            let decision = pause_with_io(&config, stdin, &mut stdout)
                .await
                .expect("the second answer decides");

            assert_that!(decision).is_equal_to(PauseDecision::Abort);
            assert_that!(String::from_utf8(stdout).expect("output is UTF-8"))
                .is_equal_to("Paused.\nGo? Enter 'y' to continue or 'n' to abort.\nGo? ");
        }
    }
}

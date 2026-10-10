//! Cancelling runs, e.g. when the process is asked to shut down on Ctrl-C.

use std::{
    io,
    sync::{LazyLock, Mutex, PoisonError, mpsc},
    thread,
};

use chrome_for_testing_manager::CancellationToken;
use rootcause::{Report, prelude::ResultExt as _};

use crate::BrowserTestError;

/// Exit status of a process that exited on a repeated shutdown signal, following the shell's
/// `128 + SIGINT` convention.
const FORCED_EXIT_STATUS: i32 = 130;

/// Cancelled on the first shutdown signal, once the signals are listened for. Shared by every
/// [`Cancellation::on_shutdown_signals`], as the signals concern the whole process.
static SHUTDOWN: LazyLock<CancellationToken> = LazyLock::new(CancellationToken::new);

/// Whether the process listens for the shutdown signals yet.
static LISTENING: Mutex<bool> = Mutex::new(false);

/// How the runs of a [`crate::BrowserTestRunner`] are cancelled, e.g. on Ctrl-C. Required by
/// [`crate::BrowserTestRunner::new`].
///
/// `ChromeDriver` and the browsers it starts run in a process group of their own. A run that is
/// interrupted without being cancelled, e.g. by Ctrl-C ending the process, leaves them running.
/// A cancelled run instead starts no further tests, cancels running ones, quits their sessions,
/// and shuts down `ChromeDriver`, which ends every browser of the run. It also removes the Chrome
/// profiles of its sessions. [`crate::BrowserTestRunner::run`] then returns
/// [`BrowserTestError::Cancelled`]. Once cancelled, every later run of the runner with tests is
/// cancelled right away.
///
/// - [`Self::on_shutdown_signals`] cancels runs on Ctrl-C or SIGTERM. Use it unless your
///   application handles these signals itself.
/// - [`Self::from_token`] cancels runs once a token of your own is cancelled, e.g. your
///   application's shutdown token.
/// - [`Self::disabled`] never cancels runs.
///
/// ```no_run
/// use browser_test::{BrowserTestRunner, Cancellation};
///
/// let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals());
/// ```
#[derive(Debug, Clone)]
pub struct Cancellation {
    token: CancellationToken,
    /// Whether runs need the process to listen for the shutdown signals, see
    /// [`Self::on_shutdown_signals`].
    shutdown_signals: bool,
}

impl Cancellation {
    /// Cancel runs on the signals asking the process to shut down.
    ///
    /// On Unix, these are SIGINT (Ctrl-C) and SIGTERM, which tools like `kill`, `timeout`, and CI
    /// runners send. On Windows, it is Ctrl-C. A second signal exits the process right away, with
    /// status 130, in case the shutdown hangs.
    ///
    /// The process listens for the signals once, from the first run with tests of any runner using
    /// this on, on a thread of its own. Until then, the signals end the process as usual. From then on,
    /// they no longer end the process on their own, for the rest of the process. Call this as
    /// often as you like: every runner using it is cancelled on the first signal, also those
    /// running no tests at the time, so that their later runs are cancelled right away. A run
    /// fails with [`BrowserTestError::ListenForShutdownSignals`] if the signals cannot be listened
    /// for.
    ///
    /// This is a convenience for applications that do not handle these signals themselves.
    /// Handlers of your own would react to every signal next to these. Cancel a token from them
    /// instead, and pass it with [`Self::from_token`].
    #[must_use]
    pub fn on_shutdown_signals() -> Self {
        Self {
            token: SHUTDOWN.clone(),
            shutdown_signals: true,
        }
    }

    /// Cancel runs once `token` is cancelled.
    ///
    /// The runner listens for no signals itself. Cancel `token` when your application shuts down,
    /// e.g. from its own Ctrl-C handler.
    #[must_use]
    pub fn from_token(token: CancellationToken) -> Self {
        Self {
            token,
            shutdown_signals: false,
        }
    }

    /// Never cancel runs.
    ///
    /// A run interrupted by Ctrl-C or SIGTERM then ends with the process, and can leave
    /// `ChromeDriver` and its browsers running. The next run removes the Chrome profiles they
    /// leave behind.
    #[must_use]
    pub fn disabled() -> Self {
        Self::from_token(CancellationToken::new())
    }

    /// Return the `CancellationToken` of one run, listening for the shutdown signals first if
    /// requested.
    pub(crate) fn run_token(&self) -> Result<CancellationToken, Report<BrowserTestError>> {
        if self.shutdown_signals {
            listen_for_shutdown_signals()?;
        }
        // A child token, so that the run can also be cancelled on its own.
        Ok(self.token.child_token())
    }
}

/// Make the process listen for the signals asking it to shut down, cancelling [`SHUTDOWN`] on the
/// first one, unless it does already.
///
/// The listener runs on a Tokio runtime of its own, on a thread of its own. Tokio never removes
/// its signal handlers again, so a listener on the caller's runtime would leave the signals
/// ignored once that runtime ends, e.g. after the first `#[tokio::test]`.
fn listen_for_shutdown_signals() -> Result<(), Report<BrowserTestError>> {
    let mut listening = LISTENING.lock().unwrap_or_else(PoisonError::into_inner);
    if *listening {
        return Ok(());
    }
    let (registered_sender, registered) = mpsc::channel();
    thread::Builder::new()
        .name("browser-test-shutdown-signals".into())
        .spawn(move || {
            let setup = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
                .and_then(|runtime| {
                    let listener = {
                        let _entered = runtime.enter();
                        listen(SHUTDOWN.clone())?
                    };
                    Ok((runtime, listener))
                });
            match setup {
                Ok((runtime, listener)) => {
                    let _ = registered_sender.send(Ok(()));
                    runtime.block_on(listener);
                }
                Err(error) => {
                    let _ = registered_sender.send(Err(error));
                }
            }
        })
        .context(BrowserTestError::ListenForShutdownSignals)?;
    // Blocks the caller only until the signals are registered, once per process.
    registered
        .recv()
        .unwrap_or_else(|_| Err(io::Error::other("the listener thread panicked")))
        .context(BrowserTestError::ListenForShutdownSignals)?;
    *listening = true;
    Ok(())
}

/// Register the signal handlers, and return the task that cancels `cancellation` on the first
/// signal and exits the process on the second.
#[cfg(unix)]
fn listen(
    cancellation: CancellationToken,
) -> std::io::Result<impl Future<Output = ()> + Send + 'static> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    Ok(async move {
        // The listener's runtime is never shut down, so `recv` never yields `None`.
        tokio::select! {
            _ = sigint.recv() => tracing::warn!("Received SIGINT. Cancelling the browser test run..."),
            _ = sigterm.recv() => tracing::warn!("Received SIGTERM. Cancelling the browser test run..."),
        }
        cancellation.cancel();
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
        force_exit();
    })
}

/// Register the Ctrl-C handler, and return the task that cancels `cancellation` on the first
/// Ctrl-C and exits the process on the second.
#[cfg(windows)]
fn listen(
    cancellation: CancellationToken,
) -> std::io::Result<impl Future<Output = ()> + Send + 'static> {
    let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
    Ok(async move {
        // The listener's runtime is never shut down, so `recv` never yields `None`.
        ctrl_c.recv().await;
        tracing::warn!("Received Ctrl-C. Cancelling the browser test run...");
        cancellation.cancel();
        ctrl_c.recv().await;
        force_exit();
    })
}

fn force_exit() -> ! {
    tracing::warn!("Received a second shutdown signal. Exiting immediately.");
    std::process::exit(FORCED_EXIT_STATUS);
}

/// The result of a cancelled run: always an error, holding the run's own error, if any, as a
/// child.
pub(crate) fn cancelled_result(
    result: Result<(), Report<BrowserTestError>>,
) -> Result<(), Report<BrowserTestError>> {
    let mut cancelled: Report<BrowserTestError> = Report::new(BrowserTestError::Cancelled);
    if let Err(error) = result {
        cancelled
            .children_mut()
            .push(error.into_dynamic().into_cloneable());
    }
    Err(cancelled)
}

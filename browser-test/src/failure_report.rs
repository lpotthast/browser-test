//! Failure reports that say what went wrong, when, and at which line of test code.
//!
//! A failing test's report shows the following, without any extra code in the tests:
//!
//! - **Where**: the frames of the test code that led to every error ([`TestCodeFrames`]),
//!   innermost first. An error created inside a helper (a lookup, a wait) points at the test line
//!   that called it. A panic (e.g. a failed assertion) shows its location and frames as well.
//! - **When**: the test's last steps ([`RecentSteps`]): what it was doing, how far into the test,
//!   and for how long. Steps are futures marked with [`StepExt::step`](crate::StepExt::step).
//! - **What**: the error and its context chain. thirtyfour errors show their `WebDriver` message,
//!   without chromedriver's native stack trace.
//!
//! The runner installs the [`rootcause`] hooks behind this with the first run
//! ([`BrowserTestRunner::with_failure_report_hooks`](crate::BrowserTestRunner::with_failure_report_hooks)).
//! An application that installs hooks of its own adds them with [`hooks`], and the runner then
//! leaves the installation to it.

use std::{
    any::Any,
    borrow::Cow,
    collections::{HashMap, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rootcause::{
    ReportMut, ReportRef,
    handlers::{
        AttachmentFormattingPlacement, AttachmentFormattingStyle, AttachmentHandler,
        ContextFormattingStyle, FormattingFunction,
    },
    hooks::{
        Hooks,
        builtin_hooks::location::{Location, LocationHandler},
        context_formatter::ContextFormatterHook,
        report_creation::ReportCreationHook,
    },
    markers::{Dynamic, Local, ObjectMarkerFor, SendSync, Uncloneable},
    report_attachment::ReportAttachment,
};
use thirtyfour::error::WebDriverError;

use crate::{BrowserTestError, report::FormatDuration};

/// browser-test's own sources, whose frames are never test code.
const OWN_SOURCES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/");

/// The runner's source: frames from here on down called the test.
const RUNNER_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/execution/");

/// browser-test's crate, by the symbols of its frames: where their source files don't match
/// [`OWN_SOURCES`].
const OWN_CRATE: &str = "browser_test";

/// The runner's module, by the symbols of its frames: where their source files don't match
/// [`RUNNER_SOURCE`].
const RUNNER_MODULE: &str = "browser_test::execution";

/// How many steps [`RecentSteps`] keeps.
pub(crate) const RECENT_STEP_COUNT: usize = 8;

/// `hooks` with the hooks behind failure reports added: a report creation hook attaching
/// [`TestCodeFrames`] to the errors of running tests, a formatter printing [`WebDriverError`]s as
/// their `WebDriver` message, and formatters printing [`BrowserTestError`]s and message contexts
/// (`String`, `&str`) with `Display` also where a report is printed with `Debug`.
///
/// For applications that install [`rootcause`] hooks of their own:
///
/// ```no_run
/// browser_test::failure_report::hooks(rootcause::hooks::Hooks::new())
///     // ... the application's own hooks ...
///     .install()
///     .expect("hooks are installed once");
/// ```
///
/// Once these are installed, runners no longer install the hooks themselves: [`rootcause`] takes
/// only one set of hooks per process, and the returned one includes them.
#[must_use]
pub fn hooks(hooks: Hooks) -> Hooks {
    hooks
        .report_creation_hook(TestCodeFramesHook)
        .context_formatter::<WebDriverError, _>(WebDriverErrorFormatter)
        .context_formatter::<BrowserTestError, _>(DisplayFormatter)
        .context_formatter::<String, _>(DisplayFormatter)
        .context_formatter::<&'static str, _>(DisplayFormatter)
}

/// Set by [`TestCodeFramesHook`] whenever it runs: the [`hooks`] are installed.
static HOOKS_RAN: AtomicBool = AtomicBool::new(false);

/// Whether the [`hooks`] are installed, e.g. by the application with hooks of its own: creating a
/// report runs them.
fn hooks_installed() -> bool {
    drop(rootcause::report!(
        "Are browser-test's rootcause hooks installed?"
    ));
    HOOKS_RAN.load(Ordering::Relaxed)
}

/// Install [`hooks`] and the panic hook, once per process, unless the application installed the
/// hooks with its own. Called by the runner.
pub(crate) fn install(report_hooks: bool) {
    static REPORT_HOOKS: OnceLock<()> = OnceLock::new();
    static PANIC_HOOK: OnceLock<()> = OnceLock::new();
    if report_hooks {
        REPORT_HOOKS.get_or_init(|| {
            if !hooks_installed() && hooks(Hooks::new()).install().is_err() {
                tracing::warn!(
                    "Other rootcause hooks are installed already, so failure reports lack the test \
                     code's frames. Add browser-test's with `browser_test::failure_report::hooks`."
                );
            }
        });
    }
    PANIC_HOOK.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Only the panics of tests are located.
            if !crate::step::in_test() {
                previous(info);
                return;
            }
            crate::step::record_panic(PanicDetails {
                location: info.location().map(|location| {
                    format!(
                        "{}:{}:{}",
                        location.file(),
                        location.line(),
                        location.column()
                    )
                }),
                frames: Some(TestCodeFrames::capture()),
            });
            previous(info);
        }));
    });
}

/// Where a test panicked, recorded by the panic hook.
#[derive(Debug, Clone)]
pub(crate) struct PanicDetails {
    pub(crate) location: Option<String>,
    pub(crate) frames: Option<TestCodeFrames>,
}

/// The message of a panic, from its payload.
pub(crate) fn panic_payload_message(payload: &(dyn Any + Send + 'static)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "<non-string panic payload>".to_owned()
}

/// The test root: frames of files below it are test code, unless they belong to a dependency
/// ([`Dependencies`]). The workspace of the package whose tests run (Cargo's
/// `CARGO_MANIFEST_DIR`, set for `cargo test` and `cargo run`), so that helpers in other packages
/// of the workspace count, else the working directory.
fn test_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let package = std::env::var_os("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let root = workspace_root(&package).unwrap_or(package);
        normalized(&root).into_owned()
    })
}

/// The root of the Cargo workspace of the package in `package_dir`: the closest directory, from
/// `package_dir` up, whose `Cargo.toml` has a `workspace` table.
fn workspace_root(package_dir: &Path) -> Option<PathBuf> {
    package_dir
        .ancestors()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|manifest| {
                manifest
                    .lines()
                    .any(|line| line.trim_start().starts_with("[workspace"))
            })
        })
        .map(Path::to_path_buf)
}

/// Tells the sources of dependencies below the test root apart from test code: Cargo marks the
/// directories of the crates it downloads or checks out with `.cargo-ok` (registry crates and git
/// checkouts, e.g. in a `CARGO_HOME` inside the project) and of vendored crates with
/// `.cargo-checksum.json`. Sources in the target directory (generated by build scripts) are no
/// test code either.
struct Dependencies {
    target_dir: PathBuf,
    /// Whether a directory is a dependency's, by directory, as checking takes file system calls.
    dirs: Mutex<HashMap<PathBuf, bool>>,
}

impl Dependencies {
    fn new(target_dir: PathBuf) -> Self {
        Self {
            target_dir,
            dirs: Mutex::default(),
        }
    }

    fn of_test_root() -> &'static Self {
        static DEPENDENCIES: OnceLock<Dependencies> = OnceLock::new();
        DEPENDENCIES.get_or_init(|| {
            // Cargo resolves a relative `CARGO_TARGET_DIR` against the working directory.
            let target_dir = std::env::var_os("CARGO_TARGET_DIR")
                .and_then(|dir| std::path::absolute(dir).ok())
                .or_else(|| {
                    // Also finds a `build.target-dir` configured in `.cargo/config.toml`.
                    let exe = std::env::current_exe().ok()?;
                    target_dir_of(&normalized(&exe))
                })
                .unwrap_or_else(|| test_root().join("target"));
            Self::new(normalized(&target_dir).into_owned())
        })
    }

    /// Whether the source file at `path`, below `root`, belongs to a dependency.
    fn contains(&self, path: &Path, root: &Path) -> bool {
        if path.starts_with(&self.target_dir) {
            return true;
        }
        let Some(dir) = path.parent() else {
            return false;
        };
        let mut dirs = self.dirs.lock().unwrap_or_else(PoisonError::into_inner);
        Self::is_dependency_dir(&mut dirs, dir, root)
    }

    fn is_dependency_dir(dirs: &mut HashMap<PathBuf, bool>, dir: &Path, root: &Path) -> bool {
        if dir == root || !dir.starts_with(root) {
            return false;
        }
        if let Some(&known) = dirs.get(dir) {
            return known;
        }
        let marked = dir.join(".cargo-ok").exists() || dir.join(".cargo-checksum.json").exists();
        let dependency = marked
            || dir
                .parent()
                .is_some_and(|parent| Self::is_dependency_dir(dirs, parent, root));
        dirs.insert(dir.to_path_buf(), dependency);
        dependency
    }
}

/// The Cargo target directory that the executable `exe` was built into: the closest of its
/// ancestors that Cargo marked with `CACHEDIR.TAG`, else `<target>` of
/// `<target>/<profile>/deps/<exe>`, where Cargo puts tests.
fn target_dir_of(exe: &Path) -> Option<PathBuf> {
    let marked = exe
        .ancestors()
        .skip(1)
        .find(|dir| dir.join("CACHEDIR.TAG").is_file());
    let deps = || {
        let deps = exe.parent().filter(|dir| dir.ends_with("deps"))?;
        deps.parent()?.parent()
    };
    marked.or_else(deps).map(Path::to_path_buf)
}

/// The frames of the test code on the stack where an error was created or a panic happened,
/// innermost first: `tests/ui/checkbox.rs:84 in checkbox::selected_state`.
///
/// Test code is the code of the workspace whose tests run, without its dependencies, up to the
/// frame that the runner called.
///
/// The stack is captured where the error is created, but only symbolized the first time the
/// frames are needed, e.g. when the report is printed: symbolizing takes long, and many errors a
/// test creates are never printed, as the test handles them itself (e.g. while retrying).
#[derive(Clone)]
pub struct TestCodeFrames {
    captured: Arc<CapturedFrames>,
}

/// A captured stack, until it is symbolized to the test code's frames.
struct CapturedFrames {
    backtrace: Mutex<Option<backtrace::Backtrace>>,
    frames: OnceLock<Vec<Frame>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    /// The absolute path of the source file.
    path: PathBuf,
    /// The source file as shown, relative to the test root.
    file: String,
    line: u32,
    function: String,
}

impl Frame {
    /// Whether the frame is at `line` of `file`, a path as rustc reports locations (in panics and
    /// [`Location`]s): relative to the workspace root for the workspace's sources, absolute for
    /// others, e.g. registry crates.
    fn is_at(&self, file: &str, line: u32) -> bool {
        self.line == line && test_root().join(file) == self.path
    }
}

impl TestCodeFrames {
    /// The current stack, symbolized to the test code's frames when they are needed.
    pub(crate) fn capture() -> Self {
        Self {
            captured: Arc::new(CapturedFrames {
                backtrace: Mutex::new(Some(backtrace::Backtrace::new_unresolved())),
                frames: OnceLock::new(),
            }),
        }
    }

    #[cfg(test)]
    fn from_frames(frames: Vec<Frame>) -> Self {
        Self {
            captured: Arc::new(CapturedFrames {
                backtrace: Mutex::new(None),
                frames: OnceLock::from(frames),
            }),
        }
    }

    /// The test code's frames, symbolized on first use.
    fn frames(&self) -> &[Frame] {
        self.captured.frames.get_or_init(|| {
            let backtrace = self
                .captured
                .backtrace
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            backtrace.map_or_else(Vec::new, |mut backtrace| {
                backtrace.resolve();
                test_code_frames(&backtrace)
            })
        })
    }

    /// Whether the stack holds no test code.
    pub(crate) fn is_empty(&self) -> bool {
        self.frames().is_empty()
    }

    /// Whether `location` (`file:line:column`, as a panic reports it) is the innermost frame: the
    /// test code itself panicked there.
    pub(crate) fn starts_at(&self, location: &str) -> bool {
        let mut parts = location.rsplitn(3, ':');
        let (Some(_column), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        line.parse()
            .is_ok_and(|line| self.starts_at_line(file, line))
    }

    /// Whether `line` of `file` is the innermost frame.
    fn starts_at_line(&self, file: &str, line: u32) -> bool {
        self.frames()
            .first()
            .is_some_and(|frame| frame.is_at(file, line))
    }
}

/// The test code's frames of `backtrace`, a symbolized one.
///
/// browser-test's own frames are told apart by their source files and, where those don't match
/// (e.g. with remapped paths), by their symbols.
fn test_code_frames(backtrace: &backtrace::Backtrace) -> Vec<Frame> {
    let root = test_root();
    let dependencies = Dependencies::of_test_root();
    let own_sources = normalized(Path::new(OWN_SOURCES));
    let runner_source = normalized(Path::new(RUNNER_SOURCE));
    let mut frames: Vec<Frame> = Vec::new();
    // For the debug log when no frame is test code: usually the frame the runner called.
    let mut outermost_source = None;
    'frames: for frame in backtrace.frames() {
        for symbol in frame.symbols() {
            let name = symbol.name().map(|name| name.to_string());
            let name = name.as_deref().unwrap_or_default();
            let path = symbol.filename().map(|path| source_path(path, root));
            if in_module(name, RUNNER_MODULE)
                || path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&runner_source))
            {
                // The runner called the test: everything below is not test code.
                break 'frames;
            }
            let Some(path) = path else {
                continue;
            };
            if in_module(name, OWN_CRATE) || path.starts_with(&own_sources) {
                // browser-test's helpers between the test's frames (steps, this hook).
                continue;
            }
            outermost_source = Some(path.clone());
            let (Ok(file), Some(line)) = (path.strip_prefix(root), symbol.lineno()) else {
                continue;
            };
            if dependencies.contains(&path, root) {
                continue;
            }
            let entry = Frame {
                file: file.display().to_string(),
                path,
                line,
                function: function_name(name),
            };
            if frames.last() != Some(&entry) {
                frames.push(entry);
            }
        }
    }
    if frames.is_empty() {
        log_no_test_code(root, outermost_source.as_deref());
    }
    frames
}

/// Log once that a stack captured in a test held no test code, at `debug` level: the test code's
/// source files may not be where failure reports expect them.
fn log_no_test_code(root: &Path, outermost_source: Option<&Path>) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Some(source) = outermost_source {
        tracing::debug!(
            "Found no test code on the stack of an error or panic in a test: none of its frames \
             is in a source file below the test root {}, other than a dependency's. The outermost \
             frame outside browser-test is in {}.",
            root.display(),
            source.display(),
        );
    } else {
        tracing::debug!(
            "Found no test code on the stack of an error or panic in a test: its frames outside \
             browser-test have no source files. Is debug info missing?"
        );
    }
}

/// The source file `path` of a frame, as debug info names it, comparable with the test root: a
/// relative path (from `--remap-path-prefix` or `trim-paths`) joined onto the `root` if the file
/// is there, and [`normalized`].
fn source_path(path: &Path, root: &Path) -> PathBuf {
    if path.is_relative() {
        let joined = root.join(path);
        if joined.is_file() {
            return joined;
        }
    }
    normalized(path).into_owned()
}

/// `path` without a Windows verbatim prefix and with an upper-case drive letter (see
/// [`normalized_windows_path`]). Other platforms keep it as it is.
fn normalized(path: &Path) -> Cow<'_, Path> {
    if !cfg!(windows) {
        return Cow::Borrowed(path);
    }
    match path.to_str().map(normalized_windows_path) {
        Some(Cow::Owned(normalized)) => Cow::Owned(PathBuf::from(normalized)),
        _ => Cow::Borrowed(path),
    }
}

/// The Windows path `path` without a verbatim prefix (`\\?\C:\x` is `C:\x`, `\\?\UNC\host\share`
/// is `\\host\share`) and with an upper-case drive letter: debug info and Cargo may spell the same
/// path either way.
fn normalized_windows_path(path: &str) -> Cow<'_, str> {
    let has_drive =
        |path: &str| matches!(path.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic());
    let path = if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        Cow::Owned(format!(r"\\{unc}"))
    } else if let Some(local) = path.strip_prefix(r"\\?\").filter(|local| has_drive(local)) {
        Cow::Borrowed(local)
    } else {
        Cow::Borrowed(path)
    };
    if has_drive(&path) && path.as_bytes()[0].is_ascii_lowercase() {
        let mut path = path.into_owned();
        path[..1].make_ascii_uppercase();
        Cow::Owned(path)
    } else {
        path
    }
}

/// Whether the demangled symbol `name` is in `module` (e.g. `browser_test::execution`) or below
/// it. The crate may carry a hash (`browser_test[c72fec33b3453a53]::execution::run`), and a
/// method's symbol may start with its type (`<browser_test::step::Step<F> as Future>::poll`).
fn in_module(name: &str, module: &str) -> bool {
    let name = name.strip_prefix('<').unwrap_or(name);
    let (krate, below) = module.split_once("::").unwrap_or((module, ""));
    let Some(rest) = name.strip_prefix(krate) else {
        return false;
    };
    let rest = match rest.strip_prefix('[') {
        Some(hashed) => hashed.split_once(']').map_or("", |(_hash, rest)| rest),
        None => rest,
    };
    let Some(rest) = rest.strip_prefix("::") else {
        return false;
    };
    below.is_empty()
        || rest
            .strip_prefix(below)
            .is_some_and(|rest| rest.starts_with("::"))
}

impl PartialEq for TestCodeFrames {
    fn eq(&self, other: &Self) -> bool {
        self.frames() == other.frames()
    }
}

impl Eq for TestCodeFrames {}

impl fmt::Display for TestCodeFrames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self
            .frames()
            .iter()
            .map(|frame| frame.file.len() + 1 + frame.line.to_string().len())
            .max()
            .unwrap_or_default();
        write!(f, "Test code:")?;
        for frame in self.frames() {
            let location = format!("{}:{}", frame.file, frame.line);
            write!(f, "\n  {location:width$}  {}", frame.function)?;
        }
        Ok(())
    }
}

/// The readable part of a demangled symbol: its last two path segments, without closures, hashes
/// and generic arguments. A trait method is named after the implementing type.
/// `<my_tests::Page<'_> as my_tests::Actions>::css::{{closure}}::h12ab` is `Page::css`.
fn function_name(symbol: &str) -> String {
    let mut path = String::new();
    let mut depth = 0_usize;
    // Inside `<Type as Trait>`: skip from ` as ` to the closing `>`.
    let mut skipping_trait_at = None;
    let mut rest = symbol;
    while let Some(c) = rest.chars().next() {
        if depth == 1 && skipping_trait_at.is_none() && rest.starts_with(" as ") {
            skipping_trait_at = Some(depth);
            rest = &rest[4..];
            continue;
        }
        match c {
            '<' => depth += 1,
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    skipping_trait_at = None;
                }
            }
            // Generic arguments (depth 2 and deeper inside a qualified path, depth 1 elsewhere).
            _ if skipping_trait_at.is_some() => {}
            _ if depth > usize::from(symbol.starts_with('<')) => {}
            _ => path.push(c),
        }
        rest = &rest[c.len_utf8()..];
    }
    let segments: Vec<&str> = path
        .split("::")
        .map(|segment| segment.split('[').next().unwrap_or(segment))
        .filter(|segment| {
            let hash = segment.len() == 17
                && segment.starts_with('h')
                && segment[1..].chars().all(|c| c.is_ascii_hexdigit());
            !segment.is_empty() && !segment.starts_with('{') && !hash
        })
        .collect();
    let start = segments.len().saturating_sub(2);
    segments[start..].join("::")
}

// Reports are printed with `Debug` too (a `main` or test returning `Err`): attachments read the same.
impl fmt::Debug for TestCodeFrames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Debug for RecentSteps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Where a test panicked, as its failure report shows it. A panic outside the test code (in a
/// dependency, e.g. an assertion library that raises from its own code) says so, as the test
/// code's `frames` show where the test called it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PanicLocation {
    pub(crate) location: String,
    pub(crate) frames: Option<TestCodeFrames>,
}

impl fmt::Display for PanicLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outside_test_code = self
            .frames
            .as_ref()
            .is_some_and(|frames| !frames.is_empty() && !frames.starts_at(&self.location));
        if outside_test_code {
            write!(f, "Panicked outside the test code at {}", self.location)
        } else {
            write!(f, "Panicked at {}", self.location)
        }
    }
}

impl fmt::Debug for PanicLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Prints [`TestCodeFrames`] with `Display`, unless the stack holds no test code.
pub(crate) struct TestCodeFramesHandler;

impl AttachmentHandler<TestCodeFrames> for TestCodeFramesHandler {
    fn display(value: &TestCodeFrames, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(value, formatter)
    }

    fn debug(value: &TestCodeFrames, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(value, formatter)
    }

    fn preferred_formatting_style(
        value: &TestCodeFrames,
        report_formatting_function: FormattingFunction,
    ) -> AttachmentFormattingStyle {
        let placement = if value.is_empty() {
            AttachmentFormattingPlacement::Hidden
        } else {
            AttachmentFormattingPlacement::Inline
        };
        AttachmentFormattingStyle {
            placement,
            function: report_formatting_function,
            ..Default::default()
        }
    }
}

/// The source location rootcause's default hooks attached to a report created in a test, which
/// the test code's frames replace if they start there. A location elsewhere stays: an error
/// created in a macro expansion has its frame in the macro's definition but its location at the
/// macro's call site. The frames are symbolized only when needed, so this is decided when the
/// report is printed.
struct TestCodeLocation {
    location: Location,
    frames: TestCodeFrames,
}

/// Prints a [`TestCodeLocation`] as rootcause prints a [`Location`], unless the frames start
/// there.
struct TestCodeLocationHandler;

impl AttachmentHandler<TestCodeLocation> for TestCodeLocationHandler {
    fn display(value: &TestCodeLocation, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        LocationHandler::display(&value.location, formatter)
    }

    fn debug(value: &TestCodeLocation, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        LocationHandler::debug(&value.location, formatter)
    }

    fn preferred_formatting_style(
        value: &TestCodeLocation,
        report_formatting_function: FormattingFunction,
    ) -> AttachmentFormattingStyle {
        let style = LocationHandler::preferred_formatting_style(
            &value.location,
            report_formatting_function,
        );
        let Location { file, line } = value.location;
        if value.frames.starts_at_line(file, line) {
            AttachmentFormattingStyle {
                placement: AttachmentFormattingPlacement::Hidden,
                ..style
            }
        } else {
            style
        }
    }
}

/// Drops the source location rootcause attached to a report browser-test creates for itself (a
/// test failed, the run failed): it points into browser-test, not into the test.
pub(crate) fn without_own_location<C: ?Sized>(
    mut report: rootcause::Report<C>,
) -> rootcause::Report<C> {
    let attachments = report.attachments();
    let is_location = attachments.len().checked_sub(1).is_some_and(|last| {
        attachments
            .get(last)
            .is_some_and(|attachment| attachment.downcast_inner::<Location>().is_some())
    });
    if is_location {
        report.attachments_mut().pop();
    }
    report
}

/// Attaches [`TestCodeFrames`] to every report created in a running test that has no children:
/// errors where they arise, not the context added on their way up.
#[derive(Debug, Clone, Copy)]
struct TestCodeFramesHook;

impl ReportCreationHook for TestCodeFramesHook {
    fn on_local_creation(&self, report: ReportMut<'_, Dynamic, Local>) {
        HOOKS_RAN.store(true, Ordering::Relaxed);
        attach_test_code_frames(report);
    }

    fn on_sendsync_creation(&self, report: ReportMut<'_, Dynamic, SendSync>) {
        HOOKS_RAN.store(true, Ordering::Relaxed);
        attach_test_code_frames(report);
    }
}

/// See [`TestCodeFramesHook`].
fn attach_test_code_frames<T>(mut report: ReportMut<'_, Dynamic, T>)
where
    TestCodeFrames: ObjectMarkerFor<T>,
    TestCodeLocation: ObjectMarkerFor<T>,
{
    if !report.children().is_empty() || !crate::step::in_test() {
        return;
    }
    let frames = TestCodeFrames::capture();
    let attachments = report.attachments_mut();
    let location = attachments
        .len()
        .checked_sub(1)
        .and_then(|last| attachments.get(last))
        .and_then(|attachment| attachment.downcast_inner::<Location>().copied());
    if let Some(location) = location {
        // Frames that start where the location points replace it.
        attachments.pop();
        attachments.push(
            ReportAttachment::<_, T>::new_custom::<TestCodeLocationHandler>(TestCodeLocation {
                location,
                frames: frames.clone(),
            })
            .into_dynamic(),
        );
    }
    attachments
        .push(ReportAttachment::<_, T>::new_custom::<TestCodeFramesHandler>(frames).into_dynamic());
}

/// A step of a test, as [`RecentSteps`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepEvent {
    pub(crate) kind: &'static str,
    pub(crate) detail: Option<String>,
    /// Since the test started.
    pub(crate) started: Duration,
    pub(crate) duration: Duration,
    /// Whether the step's future finished, rather than being dropped before.
    pub(crate) finished: bool,
}

/// The last steps of a failed test, oldest first: what it did right before it failed, when and for
/// how long.
#[derive(Clone, PartialEq, Eq)]
pub struct RecentSteps {
    steps: Vec<StepEvent>,
    omitted: usize,
}

impl RecentSteps {
    /// The last `steps` of `total`, recorded as they ended, listed as they started: a step
    /// enclosing others ends after them, but is listed before them.
    pub(crate) fn new(steps: &VecDeque<StepEvent>, total: usize) -> Option<Self> {
        let mut listed: Vec<_> = steps.iter().cloned().collect();
        listed.sort_by_key(|step| step.started);
        (!listed.is_empty()).then(|| Self {
            steps: listed,
            omitted: total.saturating_sub(steps.len()),
        })
    }
}

impl fmt::Display for RecentSteps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Last steps (time since the test started):")?;
        if self.omitted > 0 {
            write!(f, "\n  ({} earlier steps)", self.omitted)?;
        }
        for step in &self.steps {
            let detail = step
                .detail
                .as_deref()
                .map(|detail| format!(" {detail}"))
                .unwrap_or_default();
            let unfinished = if step.finished { "" } else { ", unfinished" };
            write!(
                f,
                "\n  +{:<8} {}{detail} ({}{unfinished})",
                FormatDuration(step.started).to_string(),
                step.kind,
                FormatDuration(step.duration)
            )?;
        }
        Ok(())
    }
}

/// A bounded log of the steps of the running test.
#[derive(Debug, Default)]
pub(crate) struct StepLog {
    events: Mutex<(VecDeque<StepEvent>, usize)>,
}

impl StepLog {
    pub(crate) fn push(&self, event: StepEvent) {
        let mut guard = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let (events, total) = &mut *guard;
        if events.len() == RECENT_STEP_COUNT {
            events.pop_front();
        }
        events.push_back(event);
        *total += 1;
    }

    pub(crate) fn recent(&self) -> Option<RecentSteps> {
        let guard = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        RecentSteps::new(&guard.0, guard.1)
    }
}

/// Prints a thirtyfour error as its `WebDriver` message, without chromedriver's native stack trace
/// and session info.
#[derive(Debug, Clone, Copy)]
struct WebDriverErrorFormatter;

impl ContextFormatterHook<WebDriverError> for WebDriverErrorFormatter {
    fn display(
        &self,
        report: ReportRef<'_, WebDriverError, Uncloneable, Local>,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        let full = report.current_context().to_string();
        let mut parts: Vec<&str> = Vec::new();
        for line in full
            .lines()
            .take_while(|line| !line.trim_start().starts_with("Stacktrace:"))
            .map(str::trim)
        {
            let line = line.strip_prefix("Error: ").unwrap_or(line);
            let line = line.strip_prefix("State: ").unwrap_or(line);
            if line.is_empty()
                || line.starts_with("(Session info:")
                || line.starts_with("Status:")
                || line.starts_with("Additional info:")
                || parts.iter().any(|part| part.contains(line))
            {
                continue;
            }
            parts.push(line);
        }
        write!(formatter, "WebDriver: {}", parts.join(" "))
    }

    fn preferred_context_formatting_style(
        &self,
        _report: ReportRef<'_, WebDriverError, Uncloneable, Local>,
        _report_formatting_function: FormattingFunction,
    ) -> ContextFormattingStyle {
        ContextFormattingStyle {
            function: FormattingFunction::Display,
            ..Default::default()
        }
    }
}

/// Prints a context with `Display` also where a report is printed with `Debug` (as a `main` or
/// test returning `Err` does): messages without quotes and escapes, errors with their message.
#[derive(Debug, Clone, Copy)]
struct DisplayFormatter;

impl<C: fmt::Display + 'static> ContextFormatterHook<C> for DisplayFormatter {
    fn preferred_context_formatting_style(
        &self,
        _report: ReportRef<'_, C, Uncloneable, Local>,
        _report_formatting_function: FormattingFunction,
    ) -> ContextFormattingStyle {
        ContextFormattingStyle {
            function: FormattingFunction::Display,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    /// A frame of `file`, relative to the test root.
    fn frame(file: &str, line: u32, function: &str) -> Frame {
        Frame {
            path: test_root().join(file),
            file: file.to_owned(),
            line,
            function: function.to_owned(),
        }
    }

    #[test]
    fn a_panic_location_is_test_code_when_it_is_the_innermost_frame() {
        let frames = TestCodeFrames::from_frames(vec![
            frame(
                "my_crate/tests/ui/checkbox.rs",
                84,
                "checkbox::selected_state",
            ),
            frame("my_crate/tests/main.rs", 12, "main"),
        ]);
        let absolute = test_root().join("my_crate/tests/ui/checkbox.rs:84:9");
        assert_that!(frames.starts_at("my_crate/tests/ui/checkbox.rs:84:9")).is_true();
        assert_that!(frames.starts_at(&absolute.display().to_string())).is_true();
        assert_that!(frames.starts_at("tests/ui/checkbox.rs:84:9")).is_false();
        assert_that!(frames.starts_at("my_crate/tests/ui/checkbox.rs:85:9")).is_false();
        assert_that!(frames.starts_at("my_crate/tests/main.rs:12:5")).is_false();
        assert_that!(frames.starts_at("no location")).is_false();
    }

    #[test]
    fn a_location_in_a_dependency_is_no_test_code_even_where_the_paths_overlap() {
        // A single-package workspace, whose `src/lib.rs` ends the path of a registry crate's.
        let frames = TestCodeFrames::from_frames(vec![frame("src/lib.rs", 84, "lib::check")]);
        assert_that!(frames.starts_at("src/lib.rs:84:9")).is_true();
        assert_that!(frames.starts_at(
            "/home/me/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/assertr-0.9.0/src/lib.rs:84:9"
        ))
        .is_false();
        assert_that!(frames.starts_at_line("/home/me/.cargo/git/checkouts/x/src/lib.rs", 84))
            .is_false();
    }

    #[test]
    fn windows_paths_lose_verbatim_prefixes_and_get_upper_case_drives() {
        for (path, normalized) in [
            (r"\\?\C:\work\app\tests\ui.rs", r"C:\work\app\tests\ui.rs"),
            (r"c:\work\app\tests\ui.rs", r"C:\work\app\tests\ui.rs"),
            (r"\\?\d:\work\app", r"D:\work\app"),
            (r"\\?\UNC\host\share\app", r"\\host\share\app"),
            (r"C:\work\app", r"C:\work\app"),
            (r"\\host\share\app", r"\\host\share\app"),
            ("/home/me/app", "/home/me/app"),
        ] {
            assert_that!(normalized_windows_path(path).into_owned()).is_equal_to(normalized);
        }
    }

    #[test]
    fn relative_source_paths_are_joined_onto_the_root_if_the_file_is_there() {
        let tree = TempTree::new("relative-sources", &["app/tests/ui.rs"]);
        let root = &tree.0;
        assert_that!(source_path(Path::new("app/tests/ui.rs"), root))
            .is_equal_to(root.join("app/tests/ui.rs"));
        // A remapped dependency, as `trim-paths` names it.
        assert_that!(source_path(Path::new("thirtyfour-0.38.1/src/lib.rs"), root))
            .is_equal_to(PathBuf::from("thirtyfour-0.38.1/src/lib.rs"));
        assert_that!(source_path(&root.join("app/tests/ui.rs"), root))
            .is_equal_to(root.join("app/tests/ui.rs"));
    }

    #[test]
    fn own_frames_are_recognized_by_their_symbols() {
        for name in [
            "browser_test::execution::test_run::run_body::{{closure}}",
            "browser_test[c72fec33b3453a53]::execution::test_run::run_body",
            "<browser_test::execution::Foo as core::future::Future>::poll",
        ] {
            assert_that!(in_module(name, RUNNER_MODULE)).is_true();
        }
        assert_that!(in_module(
            "<browser_test::step::Step<F> as core::future::Future>::poll",
            OWN_CRATE
        ))
        .is_true();
        for name in [
            "browser_test::step::record_step",
            "browser_test::executions::run",
            "browser_test_helpers::execution::run",
            "<browser_runner::MissingElementTest as browser_test::test_case::BrowserTest<()>>::run",
            "",
        ] {
            assert_that!(in_module(name, RUNNER_MODULE)).is_false();
        }
        assert_that!(in_module("browser_test_helpers::find", OWN_CRATE)).is_false();
    }

    #[test]
    fn frames_are_symbolized_when_first_needed() {
        let frames = TestCodeFrames::capture();
        assert_that!(frames.captured.frames.get()).is_none();

        // This crate's own frames are no test code.
        assert_that!(frames.is_empty()).is_true();
        assert_that!(frames.captured.frames.get()).is_some();
        assert_that!(
            frames
                .captured
                .backtrace
                .lock()
                .expect("not poisoned")
                .is_none()
        )
        .is_true();
    }

    /// A directory tree for one test, removed when dropped.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(name: &str, files: &[&str]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "browser-test-failure-report-tests-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            for file in files {
                let path = root.join(file);
                std::fs::create_dir_all(path.parent().expect("files are in directories"))
                    .expect("dir should be created");
                std::fs::write(&path, "[workspace]\n").expect("file should be written");
            }
            Self(root)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_test_root_is_the_workspace_of_the_package() {
        let tree = TempTree::new("workspace", &["Cargo.toml", "app/src/lib.rs"]);
        let root = &tree.0;
        assert_that!(workspace_root(&root.join("app"))).is_equal_to(Some(root.clone()));
        assert_that!(workspace_root(Path::new("/"))).is_none();
    }

    #[test]
    fn sources_of_dependencies_below_the_test_root_are_no_test_code() {
        let tree = TempTree::new(
            "dependencies",
            &[
                "tests/ui.rs",
                ".cargo/registry/src/index/assertr-1.0.0/.cargo-ok",
                ".cargo/registry/src/index/assertr-1.0.0/src/lib.rs",
                "vendor/thirtyfour/.cargo-checksum.json",
                "vendor/thirtyfour/src/session/handle.rs",
                "target/debug/build/app-1234/out/generated.rs",
            ],
        );
        let root = &tree.0;
        let dependencies = Dependencies::new(root.join("target"));
        assert_that!(dependencies.contains(&root.join("tests/ui.rs"), root)).is_false();
        for dependency in [
            ".cargo/registry/src/index/assertr-1.0.0/src/lib.rs",
            "vendor/thirtyfour/src/session/handle.rs",
            "target/debug/build/app-1234/out/generated.rs",
        ] {
            assert_that!(dependencies.contains(&root.join(dependency), root)).is_true();
        }
    }

    #[test]
    fn the_target_dir_is_found_from_the_executable() {
        let tree = TempTree::new(
            "target-dir",
            &[
                "build/CACHEDIR.TAG",
                "build/debug/deps/ui-0123",
                "build/debug/examples/demo",
                "unmarked/release/deps/ui-0123",
                "bin/tool",
            ],
        );
        let root = &tree.0;
        for exe in ["build/debug/deps/ui-0123", "build/debug/examples/demo"] {
            assert_that!(target_dir_of(&root.join(exe))).is_equal_to(Some(root.join("build")));
        }
        assert_that!(target_dir_of(&root.join("unmarked/release/deps/ui-0123")))
            .is_equal_to(Some(root.join("unmarked")));
        assert_that!(target_dir_of(&root.join("bin/tool"))).is_none();
    }

    #[test]
    fn recent_steps_are_listed_as_they_started() {
        let step = |kind, started| StepEvent {
            kind,
            detail: None,
            started: Duration::from_millis(started),
            duration: Duration::from_millis(1),
            finished: true,
        };
        // `login` encloses `find`, which ends first.
        let steps = VecDeque::from([step("goto", 0), step("find", 364), step("login", 200)]);
        let recent = RecentSteps::new(&steps, 3).expect("steps were recorded");
        let kinds: Vec<_> = recent.steps.iter().map(|step| step.kind).collect();
        assert_that!(kinds).is_equal_to(vec!["goto", "login", "find"]);
    }

    #[test]
    fn function_names_are_shortened() {
        assert_that!(function_name(
            "<my_tests::pages::Page<'_> as my_tests::pages::Actions>::css::{{closure}}::h0123456789abcdef"
        ))
        .is_equal_to("Page::css");
        assert_that!(function_name(
            "<browser_runner::MissingElementTest as browser_test::test_case::BrowserTest<browser_runner::IntegrationContext>>::run::{{closure}}"
        ))
        .is_equal_to("MissingElementTest::run");
        assert_that!(function_name(
            "browser_test[c72fec33b3453a53]::ui_tests::checkbox::selected_state::{{closure}}"
        ))
        .is_equal_to("checkbox::selected_state");
        assert_that!(function_name(
            "my_tests::pages::Actions::eval::<()>::{{closure}}"
        ))
        .is_equal_to("Actions::eval");
    }

    #[test]
    fn panic_payload_message_handles_common_payload_types() {
        assert_that!(panic_payload_message(&"static")).is_equal_to("static");
        assert_that!(panic_payload_message(&"owned".to_owned())).is_equal_to("owned");
        assert_that!(panic_payload_message(&42usize)).is_equal_to("<non-string panic payload>");
    }
}

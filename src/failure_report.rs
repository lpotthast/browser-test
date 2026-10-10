//! Failure reports that say what went wrong, when, and at which line of test code.
//!
//! A failing test's report shows, without anything in the test code:
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
    collections::{HashMap, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rootcause::{
    ReportMut, ReportRef,
    handlers::{ContextFormattingStyle, FormattingFunction},
    hooks::{Hooks, context_formatter::ContextFormatterHook, report_creation::ReportCreationHook},
    markers::{Dynamic, Local, ObjectMarkerFor, SendSync, Uncloneable},
    report_attachment::ReportAttachment,
};
use thirtyfour::error::WebDriverError;

use crate::{BrowserTestError, report::FormatDuration};

/// browser-test's own sources, whose frames are never test code.
const OWN_SOURCES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/");

/// The runner's source: frames from here on down called the test.
const RUNNER_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/execution/");

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
/// Once this was called, runners no longer install the hooks themselves: [`rootcause`] takes only
/// one set of hooks per process, and the returned one includes them.
#[must_use]
pub fn hooks(hooks: Hooks) -> Hooks {
    ADDED_BY_APPLICATION.store(true, Ordering::Relaxed);
    hooks
        .report_creation_hook(TestCodeFramesHook)
        .context_formatter::<WebDriverError, _>(WebDriverErrorFormatter)
        .context_formatter::<BrowserTestError, _>(DisplayFormatter)
        .context_formatter::<String, _>(DisplayFormatter)
        .context_formatter::<&'static str, _>(DisplayFormatter)
}

/// Whether the application added the hooks to its own with [`hooks`].
static ADDED_BY_APPLICATION: AtomicBool = AtomicBool::new(false);

/// Install [`hooks`] and the panic hook, once per process, unless the application added the hooks
/// to its own. Called by the runner.
pub(crate) fn install(report_hooks: bool) {
    static REPORT_HOOKS: OnceLock<()> = OnceLock::new();
    static PANIC_HOOK: OnceLock<()> = OnceLock::new();
    if report_hooks && !ADDED_BY_APPLICATION.load(Ordering::Relaxed) {
        REPORT_HOOKS.get_or_init(|| {
            if hooks(Hooks::new()).install().is_err() {
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
            crate::step::record_panic(PanicDetails {
                location: info.location().map(|location| {
                    format!(
                        "{}:{}:{}",
                        location.file(),
                        location.line(),
                        location.column()
                    )
                }),
                frames: TestCodeFrames::capture(),
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
        workspace_root(&package).unwrap_or(package)
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
                .unwrap_or_else(|| test_root().join("target"));
            Self::new(target_dir)
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

/// The frames of the test code on the stack where an error was created or a panic happened,
/// innermost first: `tests/ui/checkbox.rs:84 in checkbox::selected_state`.
///
/// Test code is the code of the workspace whose tests run, without its dependencies, up to the
/// frame that the runner called.
#[derive(Clone, PartialEq, Eq)]
pub struct TestCodeFrames {
    frames: Vec<Frame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    file: String,
    line: u32,
    function: String,
}

impl TestCodeFrames {
    /// The test code's frames of the current stack, if any.
    pub(crate) fn capture() -> Option<Self> {
        let root = test_root();
        let dependencies = Dependencies::of_test_root();
        let backtrace = backtrace::Backtrace::new();
        let mut frames: Vec<Frame> = Vec::new();
        'frames: for frame in backtrace.frames() {
            for symbol in frame.symbols() {
                let Some(path) = symbol.filename() else {
                    continue;
                };
                if path.starts_with(RUNNER_SOURCE) {
                    // The runner called the test: everything below is not test code.
                    break 'frames;
                }
                if path.starts_with(OWN_SOURCES) {
                    // browser-test's helpers between the test's frames (steps, this hook).
                    continue;
                }
                let (Ok(file), Some(line)) = (path.strip_prefix(root), symbol.lineno()) else {
                    continue;
                };
                if dependencies.contains(path, root) {
                    continue;
                }
                let entry = Frame {
                    file: file.display().to_string(),
                    line,
                    function: symbol
                        .name()
                        .map(|name| function_name(&name.to_string()))
                        .unwrap_or_default(),
                };
                if frames.last() != Some(&entry) {
                    frames.push(entry);
                }
            }
        }
        (!frames.is_empty()).then_some(Self { frames })
    }
}

impl TestCodeFrames {
    /// Whether `location` (`file:line:column`, as a panic reports it) is the innermost frame: the
    /// test code itself panicked there.
    pub(crate) fn starts_at(&self, location: &str) -> bool {
        let mut parts = location.rsplitn(3, ':');
        let (Some(_column), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        self.frames.first().is_some_and(|frame| {
            line.parse() == Ok(frame.line) && Path::new(file).ends_with(&frame.file)
        })
    }
}

impl fmt::Display for TestCodeFrames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self
            .frames
            .iter()
            .map(|frame| frame.file.len() + 1 + frame.line.to_string().len())
            .max()
            .unwrap_or_default();
        write!(f, "Test code:")?;
        for frame in &self.frames {
            let location = format!("{}:{}", frame.file, frame.line);
            write!(f, "\n  {location:width$}  {}", frame.function)?;
        }
        Ok(())
    }
}

/// The readable part of a demangled symbol: its last two path segments, without closures, hashes
/// and generic arguments; a trait method is named after the implementing type.
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
/// code's frames show where the test called it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PanicLocation {
    pub(crate) location: String,
    pub(crate) outside_test_code: bool,
}

impl fmt::Display for PanicLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.outside_test_code {
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

/// Drops the source location rootcause's default hooks attached last, if `frames` start there.
/// A location elsewhere stays: an error created in a macro expansion has its frame in the
/// macro's definition but its location at the macro's call site.
fn pop_location_at<T>(report: &mut ReportMut<'_, Dynamic, T>, frames: &TestCodeFrames) {
    let attachments = report.attachments_mut();
    let is_innermost_frame = attachments.len().checked_sub(1).is_some_and(|last| {
        attachments.get(last).is_some_and(|attachment| {
            attachment
                .downcast_inner::<rootcause::hooks::builtin_hooks::location::Location>()
                .is_some_and(|location| {
                    frames.frames.first().is_some_and(|frame| {
                        frame.line == location.line
                            && Path::new(location.file).ends_with(&frame.file)
                    })
                })
        })
    });
    if is_innermost_frame {
        attachments.pop();
    }
}

/// Drops the source location rootcause attached to a report browser-test creates for itself (a
/// test failed, the run failed): it points into browser-test, not into the test.
pub(crate) fn without_own_location<C: ?Sized>(
    mut report: rootcause::Report<C>,
) -> rootcause::Report<C> {
    let attachments = report.attachments();
    let is_location = attachments.len().checked_sub(1).is_some_and(|last| {
        attachments.get(last).is_some_and(|attachment| {
            attachment
                .downcast_inner::<rootcause::hooks::builtin_hooks::location::Location>()
                .is_some()
        })
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
        attach_test_code_frames(report);
    }

    fn on_sendsync_creation(&self, report: ReportMut<'_, Dynamic, SendSync>) {
        attach_test_code_frames(report);
    }
}

/// See [`TestCodeFramesHook`].
fn attach_test_code_frames<T>(mut report: ReportMut<'_, Dynamic, T>)
where
    TestCodeFrames: ObjectMarkerFor<T>,
{
    if report.children().is_empty()
        && crate::step::in_test()
        && let Some(frames) = TestCodeFrames::capture()
    {
        // Frames that start where the location points replace it.
        pop_location_at(&mut report, &frames);
        report.attachments_mut().push(
            ReportAttachment::<_, T>::new_custom::<rootcause::handlers::Display>(frames)
                .into_dynamic(),
        );
    }
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

    #[test]
    fn a_panic_location_is_test_code_when_it_is_the_innermost_frame() {
        let frames = TestCodeFrames {
            frames: vec![
                Frame {
                    file: "tests/ui/checkbox.rs".to_owned(),
                    line: 84,
                    function: "checkbox::selected_state".to_owned(),
                },
                Frame {
                    file: "tests/main.rs".to_owned(),
                    line: 12,
                    function: "main".to_owned(),
                },
            ],
        };
        assert!(frames.starts_at("tests/ui/checkbox.rs:84:9"));
        assert!(frames.starts_at("my_crate/tests/ui/checkbox.rs:84:9"));
        assert!(!frames.starts_at("tests/ui/checkbox.rs:85:9"));
        assert!(!frames.starts_at("tests/main.rs:12:5"));
        assert!(!frames.starts_at("/home/me/.cargo/registry/src/assertr/src/lib.rs:84:9"));
        assert!(!frames.starts_at("no location"));
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
            "<browser_runner::MissingElementTest as browser_test::test_case::BrowserTest<browser_runner::IntegrationContext, browser_runner::IntegrationTestError>>::run::{{closure}}"
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
}

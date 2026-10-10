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
//! An application that installs hooks of its own adds them with [`hooks`].

use std::{
    collections::VecDeque,
    fmt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
    time::Duration,
};

use rootcause::{
    ReportMut, ReportRef,
    handlers::{ContextFormattingStyle, FormattingFunction},
    hooks::{Hooks, context_formatter::ContextFormatterHook, report_creation::ReportCreationHook},
    markers::{Dynamic, Local, SendSync, Uncloneable},
    report_attachment::ReportAttachment,
};
use thirtyfour::error::WebDriverError;

use crate::{BrowserTestError, report::FormatDuration};

/// browser-test's own sources, whose frames are never test code.
const OWN_SOURCES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/");

/// The runner's source: frames from here on down called the test.
const RUNNER_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/execution.rs");

/// How many steps [`RecentSteps`] keeps.
pub(crate) const RECENT_STEP_COUNT: usize = 8;

/// The hooks behind failure reports, added to `hooks`: [`TestCodeFramesHook`], the formatter of
/// [`WebDriverError`], and [`DisplayFormatter`] for [`BrowserTestError`] and message contexts
/// (`String`, `&str`). For applications that install
/// [`rootcause`] hooks of their own:
///
/// ```no_run
/// browser_test::failure_report::hooks(rootcause::hooks::Hooks::new())
///     // ... the application's own hooks ...
///     .install()
///     .expect("hooks are installed once");
/// ```
///
/// Then disable the runner's own installation with
/// [`BrowserTestRunner::with_failure_report_hooks(false)`](crate::BrowserTestRunner::with_failure_report_hooks).
#[must_use]
pub fn hooks(hooks: Hooks) -> Hooks {
    hooks
        .report_creation_hook(TestCodeFramesHook)
        .context_formatter::<WebDriverError, _>(WebDriverErrorFormatter)
        .context_formatter::<BrowserTestError, _>(DisplayFormatter)
        .context_formatter::<String, _>(DisplayFormatter)
        .context_formatter::<&'static str, _>(DisplayFormatter)
}

/// Install [`hooks`] and the panic hook, once per process. Called by the runner.
pub(crate) fn install(report_hooks: bool) {
    static REPORT_HOOKS: OnceLock<()> = OnceLock::new();
    static PANIC_HOOK: OnceLock<()> = OnceLock::new();
    if report_hooks {
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

/// The test root: frames of files below it are test code. The directory of the package whose
/// tests run (Cargo's `CARGO_MANIFEST_DIR`, set for `cargo test` and `cargo run`), else the
/// working directory.
fn test_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        std::env::var_os("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    })
}

/// The frames of the test code on the stack where an error was created or a panic happened,
/// innermost first: `tests/ui/checkbox.rs:84 in checkbox::selected_state`.
///
/// Test code is the code of the package whose tests run (see [`hooks`]), up to the frame that the
/// runner called.
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
    #[must_use]
    pub fn capture() -> Option<Self> {
        let root = test_root();
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

/// Where a test panicked, as its failure report shows it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PanicLocation(pub(crate) String);

impl fmt::Display for PanicLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Panicked at {}", self.0)
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
pub struct TestCodeFramesHook;

impl ReportCreationHook for TestCodeFramesHook {
    fn on_local_creation(&self, mut report: ReportMut<'_, Dynamic, Local>) {
        if report.children().is_empty()
            && crate::step::in_test()
            && let Some(frames) = TestCodeFrames::capture()
        {
            // Frames that start where the location points replace it.
            pop_location_at(&mut report, &frames);
            report.attachments_mut().push(
                ReportAttachment::<_, Local>::new_custom::<rootcause::handlers::Display>(frames)
                    .into_dynamic(),
            );
        }
    }

    fn on_sendsync_creation(&self, mut report: ReportMut<'_, Dynamic, SendSync>) {
        if report.children().is_empty()
            && crate::step::in_test()
            && let Some(frames) = TestCodeFrames::capture()
        {
            // Frames that start where the location points replace it.
            pop_location_at(&mut report, &frames);
            report.attachments_mut().push(
                ReportAttachment::<_, SendSync>::new_custom::<rootcause::handlers::Display>(frames)
                    .into_dynamic(),
            );
        }
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
}

/// The last steps of a failed test, oldest first: what it did right before it failed, when and for
/// how long.
#[derive(Clone, PartialEq, Eq)]
pub struct RecentSteps {
    steps: Vec<StepEvent>,
    omitted: usize,
}

impl RecentSteps {
    pub(crate) fn new(steps: &VecDeque<StepEvent>, total: usize) -> Option<Self> {
        (!steps.is_empty()).then(|| Self {
            steps: steps.iter().cloned().collect(),
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
            write!(
                f,
                "\n  +{:<8} {}{detail} ({})",
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
pub struct WebDriverErrorFormatter;

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
pub struct DisplayFormatter;

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

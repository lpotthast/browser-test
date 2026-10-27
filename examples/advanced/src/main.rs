mod tests;

use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Duration;

use browser_test::thirtyfour::ChromiumLikeCapabilities;
use browser_test::{
    BrowserTestRunReport, BrowserTestRunner, CachedData, Cancellation, Channel, ChromeBinary,
    DriverOutput, ElementQueryWait, FailurePolicy, Parallelism, Pause, ProgressWarnings,
    SessionReset, SessionReuse, StderrSummary, TestFilter, TestSuite, Timeouts, Visibility,
};
use rootcause::Report;
use rootcause::hooks::Hooks;
use rootcause::prelude::ResultExt;
use rootcause_backtrace::BacktraceCollector;
use rootcause_tracing::{RootcauseLayer, SpanCollector};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Layer, Registry};

use crate::tests::{EditionIsLinked, SearchInputIsVisible, TitleContainsWikipedia};

struct Context {
    base_url: &'static str,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Report> {
    let subscriber = Registry::default().with(RootcauseLayer).with(
        tracing_subscriber::fmt::layer()
            .with_test_writer()
            .with_filter(LevelFilter::INFO),
    );
    tracing::subscriber::set_global_default(subscriber)
        .context("Setting global tracing subscriber")?;

    let _ = rustls::crypto::ring::default_provider().install_default();

    // rootcause takes one set of hooks per process: add browser-test's failure report hooks to
    // ours.
    browser_test::failure_report::hooks(Hooks::new())
        .report_creation_hook(SpanCollector {
            capture_span_for_reports_with_children: false,
        })
        .report_creation_hook(BacktraceCollector {
            capture_backtrace_for_reports_with_children: false,
            ..BacktraceCollector::new_from_env()
        })
        .install()
        .context("Installing rootcause hooks")?;

    let context = Context {
        base_url: "https://www.wikipedia.org/",
    };

    // Four tests, two at a time: later tests run in a spare session or in the reset session of an
    // earlier test. Set e.g. `BROWSER_TEST_TAG=smoke` or `BROWSER_TEST_NAME=edition` to run only
    // some of them.
    let tests = TestSuite::parallel(
        Parallelism::from_env()
            .context("Reading the parallelism setting")?
            .unwrap_or(Parallelism::parallel(2)),
    )
    .with(TitleContainsWikipedia)
    .with(SearchInputIsVisible)
    .with(EditionIsLinked { language: "en" })
    .with(EditionIsLinked { language: "de" })
    .filter(&TestFilter::from_env().context("Reading the test filter")?);

    // Set `BROWSER_TEST_VISIBLE=0` to run headless, in Chrome Headless Shell.
    let visibility = Visibility::from_env()
        .context("Reading the visibility setting")?
        .unwrap_or(Visibility::Visible);

    // Keep downloads and profiles in the example's `target` directory, whatever the current
    // directory is. Chrome for Testing is then downloaded once for this example, instead of being
    // shared through the user's cache directory.
    let target_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");

    BrowserTestRunner::new(Cancellation::on_shutdown_signals())
        .with_channel(Channel::Stable)
        .with_visibility(visibility)
        .with_headless_chrome_binary(ChromeBinary::ChromeHeadlessShell)
        .with_chrome_capabilities(|caps| caps.add_arg("--window-size=1280,900"))
        .with_chrome_for_testing_cache_dir(target_dir.join("chrome-for-testing"))
        .with_chrome_profiles_dir(target_dir.join("browser-test-profiles"))
        // We installed browser-test's hooks together with ours above.
        .with_failure_report_hooks(false)
        .with_pause(
            Pause::from_env()
                .context("Reading the pause setting")?
                .unwrap_or_default()
                .with_message("Paused before the Wikipedia tests.")
                .with_prompt("Run the tests? [y/N] ")
                .with_hint(format!("Wikipedia is available at {}", context.base_url)),
        )
        .with_timeouts(
            Timeouts::new()
                .with_script(Duration::from_secs(5))
                .with_page_load(Duration::from_secs(10))
                .with_implicit_wait(Duration::ZERO),
        )
        .with_element_query_wait(ElementQueryWait::new(
            Duration::from_secs(10),
            Duration::from_millis(500),
        ))
        .with_failure_policy(FailurePolicy::RunAll)
        .with_driver_output(
            DriverOutput::from_env()
                .context("Reading the driver output setting")?
                .unwrap_or(DriverOutput::tail_lines(100)),
        )
        .with_session_reuse(
            SessionReuse::from_env()
                .context("Reading the session reuse setting")?
                .unwrap_or(SessionReuse::enabled())
                // Wikipedia serves the same resources throughout the run.
                .with_reset(SessionReset::manual([CachedData::Http]))
                .with_max_tests_per_session(NonZeroUsize::new(10).unwrap())
                .with_back_forward_cache(false),
        )
        // A spare browser window would open on top of the window of the running test.
        .with_spare_sessions(match visibility {
            Visibility::Visible => 0,
            Visibility::Headless => 1,
        })
        .with_progress_warnings(
            ProgressWarnings::default()
                .with_test_running(Some(Duration::from_secs(20)))
                .with_session(Some(Duration::from_secs(10)))
                .with_slow_step(Some(Duration::from_secs(3))),
        )
        .with_report_consumer(StderrSummary)
        .with_report_consumer(|report: &BrowserTestRunReport| {
            tracing::info!(
                sessions_created = report.sessions_created(),
                session_resets = report.session_resets(),
                "Browser sessions used",
            );
        })
        .run(&context, tests)
        .await
        .context("Running browser tests")?;

    Ok(())
}

mod tests;

use std::time::Duration;

use browser_test::{
    BrowserTestRunner, BrowserTests, DriverOutput, ElementQueryWait, Parallelism, Pause,
    StderrSummary, Visibility,
};
use rootcause::Report;
use rootcause::hooks::Hooks;
use rootcause::prelude::ResultExt;
use rootcause_backtrace::BacktraceCollector;
use rootcause_tracing::{RootcauseLayer, SpanCollector};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Layer, Registry};

use crate::tests::{SearchInputIsVisible, TitleContainsWikipedia};

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

    Hooks::new()
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

    let tests = BrowserTests::parallel(Parallelism::parallel(2))
        .with(TitleContainsWikipedia)
        .with(SearchInputIsVisible);

    BrowserTestRunner::new()
        .with_visibility(Visibility::Visible)
        .with_pause(
            Pause::from_env()
                .context("Reading the pause setting")?
                .unwrap_or_default()
                .with_hint(format!("Wikipedia is available at {}", context.base_url)),
        )
        .with_timeouts(
            browser_test::Timeouts::builder()
                .script_timeout(Duration::from_secs(5))
                .page_load_timeout(Duration::from_secs(10))
                .implicit_wait_timeout(Duration::from_secs(0))
                .build(),
        )
        .with_element_query_wait(
            ElementQueryWait::new(Duration::from_secs(10), Duration::from_millis(500))
                .context("Configuring element query waits")?,
        )
        .with_driver_output(DriverOutput::tail_lines(100))
        .with_report_consumer(StderrSummary)
        .run(&context, tests)
        .await
        .context("Running browser tests")?;

    Ok(())
}

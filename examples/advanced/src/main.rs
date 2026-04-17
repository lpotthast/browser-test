mod tests;

use std::num::NonZeroUsize;
use std::time::Duration;

use browser_test::{
    BrowserTestParallelism, BrowserTestRunner, BrowserTestVisibility, BrowserTests,
    DriverOutputConfig, ElementQueryWaitConfig, PauseConfig,
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

    let tests = BrowserTests::new()
        .with(TitleContainsWikipedia)
        .with(SearchInputIsVisible);

    BrowserTestRunner::new()
        .with_visibility(BrowserTestVisibility::Visible)
        .with_pause(PauseConfig::from_env())
        .with_timeouts(
            browser_test::BrowserTimeouts::builder()
                .script_timeout(Duration::from_secs(5))
                .page_load_timeout(Duration::from_secs(10))
                .implicit_wait_timeout(Duration::from_secs(0))
                .build(),
        )
        .with_element_query_wait(
            ElementQueryWaitConfig::builder()
                .timeout(Duration::from_secs(10))
                .interval(Duration::from_millis(500))
                .build(),
        )
        .with_driver_output(DriverOutputConfig::new(100))
        .with_test_parallelism(BrowserTestParallelism::Parallel(
            NonZeroUsize::new(2).expect("parallelism should be non-zero"),
        ))
        .with_hint(format!("Wikipedia is available at {}", context.base_url))
        .run(&context, tests)
        .await
        .context("Running browser tests")?;

    Ok(())
}

# browser-test

`browser-test` is a small Rust crate for async browser-driven integration tests.

It is intentionally not a web-app launcher. Your test harness starts the app however it wants:
`cargo run`, `cargo leptos serve`, a Docker Compose stack, a static fixture page, or something else.

`browser-test` takes it from there: it resolves a Chrome for Testing release through
`chrome-for-testing-manager`, downloads it and starts the corresponding chromedriver automatically, then creates one
fresh `WebDriver` session per test, runs your tests, and shuts the driver down again.

This crate currently only integrates with `thirtyfour`.

## Thirtyfour API Surface

`browser-test` intentionally uses `thirtyfour` as its current `WebDriver` backend. Selected
`thirtyfour` types are re-exported by this crate and are part of its public API, so updating the
bundled `thirtyfour` version can be semver-relevant for `browser-test`.

Prefer the `browser-test` re-exports in downstream test crates:

```rust
use browser_test::{BrowserTest, BrowserTestRunner, BrowserTests};
use browser_test::thirtyfour::{By, WebDriver, prelude::*};
```

That keeps browser test code on the same `thirtyfour` version used by `browser-test`. Add a direct
`thirtyfour` dependency only when a test crate has a specific reason to manage that dependency
itself.

## What It Provides

- A `BrowserTest` trait for named, async test cases each getting executed with their own browser session.
- A `BrowserTests` collection for defining heterogeneous test lists without explicit boxing.
- A `BrowserTestRunner` that automatically resolves browser assets (e.g. Chrome for Testing), downloads them and
  starts the necessary driver (e.g. chromedriver) for session management.
- A fresh browser session for every test.
- Sequential execution by default, with bounded parallel execution when you ask for it.
- Fail-fast behavior by default, with an opt-in run-all mode that reports every failure.
- Headless test execution by default, with overrides through `BrowserTestVisibility`. If configured `from_env()`, set
  `BROWSER_TEST_VISIBLE=1` for visible runs.
- Built in wait-for-test-approval / pause behavior. If configured `from_env()`, set `BROWSER_TEST_PAUSE=1` for pausing
  before the browser tests are run, giving you time to inspect your started apps.
- Runner-level timeout and element-query wait configuration, with per-`BrowserTest` overrides.
- Chrome capability customization for adding arguments and experimental options.
- Browser-driver stdout/stderr capture on failures.

Use this crate when you already know how to start the system under test and want a focused runner for
the browser side of the integration test.

If you are testing a Leptos app, also look at `leptos-browser-test`, giving you easy to use tools to start a Leptos
application under test and managing its lifetime.

## Installation

Add `browser-test` to the test crate that owns your browser integration tests:

```toml
[dev-dependencies]
browser-test = "0.1"
rootcause = "0.12"
thiserror = "2"
tokio = { version = "1", default-features = false, features = ["macros", "rt-multi-thread"] }
```

## Minimal Example

This example opens Wikipedia, so there is no app startup to think about yet. In a real suite, the
shared context is usually your app's base URL or a small struct with whatever the tests need.

```rust,no_run
use std::borrow::Cow;

use browser_test::thirtyfour::WebDriver;
use browser_test::{
    async_trait, BrowserTest, BrowserTestError, BrowserTestRunner, BrowserTests,
    BrowserTestVisibility,
};
use rootcause::prelude::ResultExt;
use rootcause::Report;

#[derive(Debug, thiserror::Error)]
enum PageTitleError {
    #[error("failed to open the page")]
    OpenPage,

    #[error("failed to read the page title")]
    ReadTitle,

    #[error("unexpected page title: expected it to contain \"Wikipedia\", got {actual:?}")]
    UnexpectedTitleContains { actual: String },
}

struct BrowserTestContext {
    url: &'static str,
}

struct PageTitleTest;

#[async_trait]
impl BrowserTest<BrowserTestContext, PageTitleError> for PageTitleTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("page title")
    }

    async fn run(
        &self,
        driver: &WebDriver,
        context: &BrowserTestContext,
    ) -> Result<(), Report<PageTitleError>> {
        driver
            .goto(context.url)
            .await
            .context(PageTitleError::OpenPage)?;

        let title = driver.title().await.context(PageTitleError::ReadTitle)?;
        if !title.contains("Wikipedia") {
            return Err(Report::new(PageTitleError::UnexpectedTitleContains { actual: title }));
        }

        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_tests() -> Result<(), Report<BrowserTestError>> {
    let context = BrowserTestContext {
        url: "https://www.wikipedia.org/",
    };
    let tests = BrowserTests::new().with(PageTitleTest);

    BrowserTestRunner::new()
        .with_visibility(BrowserTestVisibility::Visible)
        .run(&context, tests)
        .await
}
```

The test error type is yours. `BrowserTestRunner::run(...)` returns
`Report<BrowserTestError>` so runner failures, test failures, and panics get useful context.

The repository checkout also includes this example as a standalone Cargo crate:

```sh
cargo run --manifest-path examples/minimal/Cargo.toml
```

## Advanced Runner Setup

Most projects eventually want more than a plain runner call. The advanced example uses the same
shape as the `leptos-tiptap` integration tests: dynamic `rootcause::Report` test errors, tracing
spans for browser steps, rootcause span/backtrace collectors, explicit timeout constants, bounded
browser-driver output capture, and parallel sessions.

From the repository checkout, run it with:

```sh
cargo run --manifest-path examples/advanced/Cargo.toml
```

The setup looks like this in outline:

```rust,ignore
use std::num::NonZeroUsize;
use std::time::Duration;

use browser_test::{
    BrowserTest, BrowserTestParallelism, BrowserTestRunner, BrowserTests,
    BrowserTestVisibility, ElementQueryWaitConfig, PauseConfig, BrowserTimeouts,
    DriverOutputConfig,
};
use rootcause::Report;
use rootcause::hooks::Hooks;
use rootcause::prelude::ResultExt;
use rootcause_backtrace::BacktraceCollector;
use rootcause_tracing::{RootcauseLayer, SpanCollector};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Layer, Registry};

const WEBDRIVER_SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);
const WEBDRIVER_PAGE_LOAD_TIMEOUT: Duration = Duration::from_secs(10);
const WEBDRIVER_IMPLICIT_WAIT_TIMEOUT: Duration = Duration::from_secs(0);
const ELEMENT_QUERY_TIMEOUT: Duration = Duration::from_secs(10);
const ELEMENT_QUERY_INTERVAL: Duration = Duration::from_millis(500);

async fn run_browser_tests(
    base_url: &str,
    tests: BrowserTests<str>,
) -> Result<(), Report> {
    install_diagnostics()?;

    BrowserTestRunner::new()
        .with_visibility(BrowserTestVisibility::Visible)
        .with_pause(PauseConfig::from_env())
        .with_timeouts(
            BrowserTimeouts::builder()
                .script_timeout(WEBDRIVER_SCRIPT_TIMEOUT)
                .page_load_timeout(WEBDRIVER_PAGE_LOAD_TIMEOUT)
                .implicit_wait_timeout(WEBDRIVER_IMPLICIT_WAIT_TIMEOUT)
                .build(),
        )
        .with_element_query_wait(
            ElementQueryWaitConfig::builder()
                .timeout(ELEMENT_QUERY_TIMEOUT)
                .interval(ELEMENT_QUERY_INTERVAL)
                .build(),
        )
        .with_driver_output(DriverOutputConfig::new(100))
        .with_test_parallelism(BrowserTestParallelism::Parallel(
            NonZeroUsize::new(2).expect("parallelism should be non-zero"),
        ))
        .with_hint(format!("Browser target is {base_url}"))
        .run(base_url, tests)
        .await
        .context("Running browser tests")?;

    Ok(())
}

fn install_diagnostics() -> Result<(), Report> {
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

    Ok(())
}
```

What those options mean:

- `with_visibility(BrowserTestVisibility::Visible)` opens a visible browser window for the example.
- `with_pause(PauseConfig::from_env())` reads `BROWSER_TEST_PAUSE`.
- `with_timeouts(...)` applies `WebDriver` script/page-load/implicit timeouts.
- `with_element_query_wait(...)` changes the default polling behavior for element queries.
- `with_driver_output(...)` keeps recent chromedriver stdout/stderr on failures.
- `with_test_parallelism(...)` runs multiple fresh `WebDriver` sessions at the same time.
- `with_hint(...)` adds context to the manual pause prompt.

Boolean environment flags are enabled by any value except unset, empty, `0`, `false`, `no`, or
`off`.

For env-driven local debugging, configure the runner with `BrowserTestVisibility::from_env()`,
`PauseConfig::from_env()`, and `DriverOutputConfig::from_env()`, then run:

```sh
BROWSER_TEST_VISIBLE=1 BROWSER_TEST_PAUSE=1 BROWSER_TEST_DRIVER_OUTPUT=1 cargo test -- --nocapture
```

## Requirements

All code using `browser-test` must to run under a multi-threaded Tokio runtime. Use:

```rust,no_run
#[tokio::test(flavor = "multi_thread")]
async fn my_browser_tests() {
    // ...
}
```

If startup fails with a message about Tokio's `CurrentThread` runtime, switch that test to the
multi-threaded flavor.

This requirement comes from the `chrome-for-testing-manager` dependency that we use to manage `Chrome for Testing`
installations.

## Execution Model

By default, tests run one at a time and the runner stops after the first failure. This is a good
starting point for deterministic integration tests.

When you opt into parallel execution, each test still gets its own `WebDriver` session. The
chromedriver process is shared for the run, so captured driver output can contain interleaved lines
from different sessions.

Note: Whether all of your tests can be run in parallel heavily depends on you app and its persistence / state
management. You may be forced to run some of your tests sequentially. Create a secondary runner for that which own
these tests and does not opt into parallel execution. You could also consider starting your app-under-test multiple
times on different ports. This would also require multiple runners though.

When you opt into run-all mode, the runner executes every test and returns all failures as child
reports on one aggregate `Report<BrowserTestError>`.

The runner also converts test panics into `BrowserTestError::Panic` reports. It still shuts down
chromedriver after errors or panics.

## Per-Test Overrides

Not all runner defaults are final. A `BrowserTest` can override `WebDriver` timeouts or element-query wait
behavior for one test:

```rust,no_run
use std::borrow::Cow;
use std::time::Duration;

use browser_test::thirtyfour::WebDriver;
use browser_test::{async_trait, BrowserTest, ElementQueryWaitConfig, BrowserTimeouts};
use rootcause::Report;

struct SlowSearchTest;

#[async_trait]
impl BrowserTest<String> for SlowSearchTest {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("slow search")
    }

    fn timeouts(&self) -> Option<BrowserTimeouts> {
        Some(
            BrowserTimeouts::builder()
                .script_timeout(Duration::from_secs(30))
                .page_load_timeout(Duration::from_secs(30))
                .implicit_wait_timeout(Duration::from_secs(0))
                .build(),
        )
    }

    fn element_query_wait(&self) -> Option<ElementQueryWaitConfig> {
        Some(
            ElementQueryWaitConfig::builder()
                .timeout(Duration::from_secs(30))
                .interval(Duration::from_millis(500))
                .build(),
        )
    }

    async fn run(&self, _driver: &WebDriver, _base_url: &String) -> Result<(), Report> {
        Ok(())
    }
}
```

## Leptos Projects

Use `browser-test` directly when your test crate already owns app startup.

Use `leptos-browser-test` when you want the Leptos test app lifecycle handled for you. It starts the
test app, waits for the listening socket, keeps recent app stdout/stderr for startup failures, and
then hands the base URL to `BrowserTestRunner`.

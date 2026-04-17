# browser-test

`browser-test` is a small Rust crate for async browser-driven integration tests.

It does not start or wait for your web app. Your test harness does that first, using `cargo run`,
`cargo leptos serve`, Docker Compose, a static fixture page, or anything else.

Then `browser-test` resolves Chrome for Testing, starts the matching chromedriver, creates one fresh `WebDriver`
session per test, runs your `thirtyfour` test code, and shuts the driver down again.

Use this crate when your project already owns app startup and wants a focused runner for the browser
side of the integration test.

## What It Does

- Manages Chrome for Testing and chromedriver through `chrome-for-testing-manager`.
- Runs named async `BrowserTest` values collected in `BrowserTests`.
- Gives every test a fresh `WebDriver` session.
- Runs sequentially and fails fast by default.
- Supports bounded parallel runs, run-all failure reporting, visible Chrome, manual pauses,
  `WebDriver` timeouts, element-query wait configuration, Chrome capability customization, and recent
  browser-driver output on failures.

## Installation

Add `browser-test` to the crate that owns your browser integration tests:

```toml
[dev-dependencies]
browser-test = "0.1"
rootcause = "0.12"
tokio = { version = "1", default-features = false, features = ["macros", "rt-multi-thread"] }
```

`browser-test` currently uses `thirtyfour` as its `WebDriver` backend. Prefer the re-exported `thirtyfour` types so your
tests use the same version as the runner:

```rust
use browser_test::{BrowserTest, BrowserTestRunner, BrowserTests};
use browser_test::thirtyfour::{By, WebDriver, prelude::*};
```

Add a direct `thirtyfour` dependency only if your test crate needs to manage that dependency
itself.

## Minimal Test

This example opens Wikipedia. In a real integration test, the shared context is usually your app's
base URL or a small struct with whatever the tests need.

```rust,no_run
use std::borrow::Cow;

use browser_test::thirtyfour::WebDriver;
use browser_test::{
    BrowserTest, BrowserTestError, BrowserTestRunner, BrowserTestVisibility, BrowserTests,
    async_trait,
};
use rootcause::{Report, report};

struct Context {
    base_url: String,
}

struct PageTitleTest;

#[async_trait]
impl BrowserTest<Context> for PageTitleTest {
    fn name(&self) -> Cow<'_, str> {
        "page title".into()
    }

    async fn run(&self, driver: &WebDriver, context: &Context) -> Result<(), Report> {
        driver.goto(&context.base_url).await?;

        let title = driver.title().await?;
        if !title.contains("Wikipedia") {
            return Err(report!(
                "unexpected page title: expected it to contain \"Wikipedia\", got {title:?}",
            ));
        }
        Ok(())
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Report<BrowserTestError>> {
    tracing_subscriber::fmt::init();

    let context = Context {
        base_url: "https://www.wikipedia.org".into(),
    };

    BrowserTestRunner::new()
        .with_visibility(BrowserTestVisibility::Visible)
        .run(&context, BrowserTests::new().with(PageTitleTest))
        .await
}

```

`BrowserTestRunner::run(...)` returns `Report<BrowserTestError>`, so runner failures, test failures,
and panics get useful context.

Browser tests must run on a multi-threaded Tokio runtime because the Chrome for Testing manager requires it.
Use `#[tokio::test(flavor = "multi_thread")]` for integration tests.

## Local Debugging

Configure the runner with environment-driven options:

```rust,no_run
use browser_test::{
    BrowserTestRunner, BrowserTestVisibility, DriverOutputConfig, PauseConfig,
};

let runner = BrowserTestRunner::new()
    .with_visibility(BrowserTestVisibility::from_env())
    .with_pause(PauseConfig::from_env())
    .with_driver_output(DriverOutputConfig::from_env());
```

Then enable them when needed:

```sh
BROWSER_TEST_VISIBLE=1 BROWSER_TEST_PAUSE=1 BROWSER_TEST_DRIVER_OUTPUT=1 cargo test -- --nocapture
```

Use `0`/`1`, `false`/`true`, `no`/`yes`, `off`/`on` or `disabled`/`enabled` for setting boolean environment flags.

## Common Configuration

Most runner options are builder methods:

```rust,no_run
use std::num::NonZeroUsize;
use std::time::Duration;

use browser_test::{
    BrowserTestFailurePolicy, BrowserTestParallelism, BrowserTestRunner, BrowserTestVisibility,
    BrowserTimeouts, DriverOutputConfig, ElementQueryWaitConfig, PauseConfig,
};

let runner = BrowserTestRunner::new()
    .with_visibility(BrowserTestVisibility::from_env())
    .with_pause(PauseConfig::from_env())
    .with_driver_output(DriverOutputConfig::from_env())
    .with_failure_policy(BrowserTestFailurePolicy::RunAll)
    .with_test_parallelism(BrowserTestParallelism::Parallel(
        NonZeroUsize::new(2).expect("parallelism should be non-zero"),
    ))
    .with_timeouts(
        BrowserTimeouts::builder()
            .script_timeout(Duration::from_secs(5))
            .page_load_timeout(Duration::from_secs(10))
            .implicit_wait_timeout(Duration::ZERO)
            .build(),
    )
    .with_element_query_wait(
        ElementQueryWaitConfig::builder()
            .timeout(Duration::from_secs(10))
            .interval(Duration::from_millis(500))
            .build(),
    );
```

A `BrowserTest` can override runner-level timeouts and element-query waits for one test by implementing `timeouts()` or
`element_query_wait()`.

## Execution Model

By default, tests run one at a time and the runner stops after the first failure. In run-all mode, the runner executes
every test and returns all failures as child reports on one aggregate `Report<BrowserTestError>`.

Parallel runs still create one fresh `WebDriver` session per test. The chromedriver process is shared for the run,
so captured driver output can contain interleaved lines from different sessions. Only enable parallelism for tests that
can safely share the same application state, or split stateful tests into a separate sequential runner.

The runner converts test panics into `BrowserTestError::Panic` reports and still shuts down chromedriver after errors
or panics.

## Examples

The repository includes runnable examples:

```sh
cargo run --manifest-path examples/minimal/Cargo.toml
cargo run --manifest-path examples/advanced/Cargo.toml
```

The advanced example shows tracing spans, rootcause span/backtrace collectors, explicit timeouts,
driver-output capture, pause prompts, and parallel sessions.

## Leptos Projects

Use `browser-test` directly when your test crate already owns app startup.

Use `leptos-browser-test` when you want the Leptos test app lifecycle handled for you. It starts the
test app, waits for the listening socket, keeps recent app stdout/stderr for startup failures, and
then hands the base URL to `BrowserTestRunner`.

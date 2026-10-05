# browser-test

[![Crates.io](https://img.shields.io/crates/v/browser-test.svg)](https://crates.io/crates/browser-test)
[![Docs.rs](https://docs.rs/browser-test/badge.svg)](https://docs.rs/browser-test)
[![CI](https://github.com/lpotthast/browser-test/actions/workflows/ci.yml/badge.svg)](https://github.com/lpotthast/browser-test/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.89.0-blue.svg)](https://github.com/lpotthast/browser-test/blob/main/Cargo.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/browser-test.svg)](#license)

`browser-test` is a small Rust crate for async browser-driven integration tests.

It does not start or wait for your web app. Your test harness does that first, using `cargo run`,
`cargo leptos serve`, Docker Compose, a static fixture page, or anything else.

Then `browser-test` resolves Chrome for Testing, starts the matching chromedriver, runs your `thirtyfour` test code in
`WebDriver` sessions that it reuses between tests (resetting them in between), reports where the time went, and shuts
the driver down again.

Use this crate when your project already owns app startup and wants a focused runner for the browser
side of the integration test.

## What It Does

- Manages Chrome for Testing and chromedriver through `chrome-for-testing-manager`.
- Runs named async `BrowserTest` values collected in `BrowserTests`.
- Reuses `WebDriver` sessions between tests, resetting browser state in between. Tests can ask for a fresh session.
- Runs sequentially and fails fast by default.
- Measures every test (session creation or reset, body, teardown) and prints a summary of each run: the slowest
  tests, the slowest steps, and the time spent on sessions. Warns when tests or sessions take unusually long.
- Supports bounded parallel runs, run-all failure reporting, visible Chrome, manual pauses,
  `WebDriver` timeouts, element-query wait configuration, Chrome capability customization, and recent
  browser-driver output on failures.

## Installation

Add `browser-test` to the crate that owns your browser integration tests:

```toml
[dev-dependencies]
browser-test = "0.4"
rootcause = "0.13"
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
    //tracing_subscriber::fmt::init();

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

Browser tests must run on a multithreaded Tokio runtime because the Chrome for Testing manager requires it.
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

You can resolve `.from_env()` configs before passing them to the runner when adjacent setup needs the resolved value.
Most runner options are builder methods:

```rust,no_run
use std::num::NonZeroUsize;
use std::time::Duration;

use browser_test::{
    BrowserTestFailurePolicy, BrowserTestParallelism, BrowserTestRunner, BrowserTestVisibility,
    BrowserTimeouts, DriverOutputConfig, ElementQueryWaitConfig, PauseConfig,
};

let browser_visibility = BrowserTestVisibility::from_env().resolve();
let run_headless = browser_visibility.is_headless();
let pause = PauseConfig::from_env().resolve();
let pause_enabled = pause.is_enabled();
let driver_output = DriverOutputConfig::from_env().resolve();
let driver_output_enabled = driver_output.is_enabled();

let runner = BrowserTestRunner::new()
    .with_visibility(browser_visibility)
    .with_pause(pause)
    .with_driver_output(driver_output)
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

In parallel runs, every parallel slot creates and reuses its own sessions. The chromedriver process is shared for the
run, so captured driver output can contain interleaved lines from different sessions. Only enable parallelism for tests
that can safely share the same application state, or split stateful tests into a separate sequential runner.

## Session Reuse

Creating a `WebDriver` session starts a new browser, which takes a noticeable share of a short test's time. So the
runner hands the session of a passing test on to the next test, after resetting it, when:

- session reuse is enabled (the default; `BrowserTestRunner::with_session_reuse(false)` gives every test a fresh
  session, `with_session_reuse(SessionReuse::from_env())` reads `BROWSER_TEST_SESSION_REUSE`),
- both tests return `SessionRequirement::Shared` from `BrowserTest::session` (the default), and
- both tests have the same effective `timeouts()` and `element_query_wait()` (these are session settings).

Tests are never reordered: a test that cannot reuse the current session ends it and starts a new one. A session is quit
(never reused) after a test failed or panicked, and after a test returning `SessionRequirement::Fresh`, which also
starts in a new session.

Before the next test runs, the reset

- opens a new blank tab (`about:blank`) and closes every other window and tab. This drops the documents of the previous
  test with all their JavaScript state, their history, `sessionStorage`, and per-tab `DevTools` overrides (e.g. emulated
  viewport sizes or media features set through CDP),
- clears the cookies of all origins,
- clears all storage (`localStorage`, `IndexedDB`, Cache Storage, service workers, ...) of every origin that a window
  showed at the end of any test of this session,
- restores the window position and size and the session's timeouts to their values after session creation, and
- runs your `SessionReset` implementations (`BrowserTestRunner::with_session_reset`), e.g. to log out of a server-side
  session.

It does not clear the HTTP cache (so reused sessions load app assets faster), storage of origins only visited in between
(or in iframes), granted permissions, or browser-wide settings changed through CDP. Tests that depend on these need
`SessionRequirement::Fresh`. If the reset fails, the runner logs a warning and gives the next test a fresh session.

## Timing and Progress

Every test's timing is logged (`tracing`, `info` level) when it finishes: how it got its session (created or reused,
and how long that took), its body, and the session teardown. At the end of each run, the runner prints a summary to
stderr (`BrowserTestRunner::with_run_summary` logs it instead or disables it):

```text
Browser test run: 39 test(s), 39 passed, 0 failed, in 2m 10.4s
  chromedriver:   started in 95ms, stopped in 1ms
  sessions:       2 created in 640ms (avg 320ms), 37 reused after resets taking 2.81s (avg 76ms), quit in 100ms
  test bodies:    2m 06.2s
  slowest tests:
       40.12s  menu_tests (reused session, reset 70ms, body 40.05s)
  ...
  slowest steps (by total time):
       52.30s  wait_for_no_selector: 18x, max 3.10s
  ...
```

`BrowserTestRunner::run_with_report` returns the same data as a `BrowserTestRunReport`.

To see which steps of a test are slow, run them through `browser_test::step`, e.g. inside your page-object helpers:

```rust,no_run
# use browser_test::thirtyfour::{WebDriver, error::WebDriverResult};
async fn goto(driver: &WebDriver, url: &str) -> WebDriverResult<()> {
    browser_test::step("goto", url, driver.goto(url)).await
}
```

Steps are logged at `debug` level with their duration, steps slower than 2 seconds at `warn` level. Each test's
`BrowserTestRecord::steps` aggregates them per kind, and the summary lists the kinds that took the most time. Each test
body also runs in a `browser_test` tracing span carrying the test name, so all logs of a test can be attributed to it.

A watchdog warns when a test is still running after 30 seconds (and every 30 seconds after that), and when creating,
resetting, or quitting a session takes longer than 5 seconds; slowness that often points at an overloaded machine or a
test waiting for something that never happens. Configure the thresholds with `BrowserTestRunner::with_progress_warnings`.

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

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.

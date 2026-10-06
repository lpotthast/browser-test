# browser-test

[![Crates.io](https://img.shields.io/crates/v/browser-test.svg)](https://crates.io/crates/browser-test)
[![Docs.rs](https://docs.rs/browser-test/badge.svg)](https://docs.rs/browser-test)
[![CI](https://github.com/lpotthast/browser-test/actions/workflows/ci.yml/badge.svg)](https://github.com/lpotthast/browser-test/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.89.0-blue.svg)](https://github.com/lpotthast/browser-test/blob/main/Cargo.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/browser-test.svg)](#license)

`browser-test` is a small Rust crate for async browser-driven integration tests.

It does not start or wait for your web app. Your test harness does that first, using `cargo run`,
`cargo leptos serve`, Docker Compose, a static fixture page, or anything else.

Then `browser-test` resolves Chrome for Testing, starts the matching chromedriver, runs your `thirtyfour` test code with
one fresh `WebDriver` session per test, reports where the time went, and shuts the driver down again.

Use this crate when your project already owns app startup and wants a focused runner for the browser
side of the integration test.

## What It Does

- Manages Chrome for Testing and chromedriver through `chrome-for-testing-manager`.
- Runs named async `BrowserTest` values collected in `BrowserTests`.
- Gives every test a fresh `WebDriver` session, created in the background while earlier tests run.
- Runs sequentially and fails fast by default.
- Measures every test (session creation, waiting for it, body, teardown) and reports each run: the slowest tests, the
  slowest steps, and the time spent on sessions, printed as a summary on request. Warns when tests or sessions take
  unusually long.
- Supports bounded parallel runs, run-all failure reporting, visible Chrome, manual pauses,
  `WebDriver` timeouts, element-query wait configuration, Chrome capability customization, and recent
  browser-driver output on failures.

## Installation

Add `browser-test` to the crate that owns your browser integration tests:

```toml
[dev-dependencies]
browser-test = "0.5"
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
    BrowserTest, BrowserTestError, BrowserTestRunner, BrowserTests, Visibility, async_trait,
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
    let context = Context {
        base_url: "https://www.wikipedia.org".into(),
    };

    BrowserTestRunner::new()
        .with_visibility(Visibility::Visible)
        .run(&context, BrowserTests::sequential().with(PageTitleTest))
        .await
}
```

`BrowserTestRunner::run(...)` returns `Report<BrowserTestError>`, so runner failures, test failures,
and panics get useful context. The runner logs through `tracing`. Install a subscriber (e.g. `tracing-subscriber`) to see
its output.

Browser tests must run on a multithreaded Tokio runtime because the Chrome for Testing manager requires it.
Use `#[tokio::test(flavor = "multi_thread")]` for integration tests.

## Local Debugging

Configure the runner with environment-driven options:

```rust,no_run
use browser_test::{BrowserTestRunner, DriverOutput, Pause, Visibility};

let runner = BrowserTestRunner::new()
    .with_visibility(Visibility::from_env()?.unwrap_or_default())
    .with_pause(Pause::from_env()?.unwrap_or_default())
    .with_driver_output(DriverOutput::from_env()?.unwrap_or_default());
# Ok::<(), browser_test::InvalidEnvVar>(())
```

`from_env()` returns `None` for an unset variable, so you decide the default, and an error for a value it cannot
interpret (e.g. `BROWSER_TEST_VISIBLE=ture`) instead of silently ignoring it. Then enable the options when needed:

```sh
BROWSER_TEST_VISIBLE=1 BROWSER_TEST_PAUSE=1 BROWSER_TEST_DRIVER_OUTPUT=1 cargo test -- --nocapture
```

| Variable                                | Read by                     | Effect                                                     |
|-----------------------------------------|-----------------------------|------------------------------------------------------------|
| `BROWSER_TEST_VISIBLE`                  | `Visibility::from_env()`    | Show the browser windows instead of running headless.      |
| `BROWSER_TEST_PAUSE`                    | `Pause::from_env()`         | Ask for confirmation before starting the browser.          |
| `BROWSER_TEST_DRIVER_OUTPUT`            | `DriverOutput::from_env()`  | Attach recent chromedriver output to errors.               |
| `BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES` | `DriverOutput::from_env()`  | Number of output lines kept (default 200).                 |
| `BROWSER_TEST_PARALLELISM`              | `Parallelism::from_env()`   | Number of tests run at the same time (see below).          |

Use `0`/`1`, `false`/`true`, `no`/`yes`, `off`/`on` or `disabled`/`enabled` for boolean environment flags (ignoring
case). Each type also has `from_env_var(name)` to read a variable of your choice.

The pause lets you inspect the app or attach a debugger before any test runs. Answer `y` to start the tests. Answering
`n` or pressing Enter ends the run successfully without starting the browser.

## Common Configuration

`from_env()` reads the environment when it is called, so the returned value can also drive adjacent setup (like
`pause_enabled` below). Runner options are builder methods:

```rust,no_run
use std::time::Duration;

use browser_test::{
    BrowserTestRunner, DriverOutput, ElementQueryWait, FailurePolicy, Pause, StderrSummary,
    Timeouts, Visibility,
};

let pause = Pause::from_env()?
    .unwrap_or_default()
    .with_hint("The app runs at http://127.0.0.1:3000");
let pause_enabled = pause.is_enabled();

let runner = BrowserTestRunner::new()
    .with_visibility(Visibility::from_env()?.unwrap_or_default())
    .with_pause(pause)
    .with_driver_output(DriverOutput::from_env()?.unwrap_or_default())
    .with_failure_policy(FailurePolicy::RunAll)
    .with_report_consumer(StderrSummary)
    .with_timeouts(
        Timeouts::builder()
            .script_timeout(Duration::from_secs(5))
            .page_load_timeout(Duration::from_secs(10))
            .implicit_wait_timeout(Duration::ZERO)
            .build(),
    )
    .with_element_query_wait(
        ElementQueryWait::new(Duration::from_secs(10), Duration::from_millis(500))
            .expect("the poll interval is non-zero"),
    );
# Ok::<(), browser_test::InvalidEnvVar>(())
```

A `BrowserTest` can override runner-level timeouts and element-query waits for one test by implementing `timeouts()` or
`element_query_wait()`.

### Chrome

The runner downloads the latest Chrome for Testing of a release channel (stable by default) and caches it in the
platform's per-user cache directory. In CI, a project-local cache directory, the smaller Chrome Headless Shell, and extra
Chrome arguments are often useful:

```rust
use browser_test::thirtyfour::ChromiumLikeCapabilities;
use browser_test::{BrowserTestRunner, Channel, ChromeBinary};

let runner = BrowserTestRunner::new()
    .with_channel(Channel::Stable)
    .with_chrome_for_testing_cache_dir("target/chrome-for-testing")
    .with_headless_chrome_binary(ChromeBinary::ChromeHeadlessShell)
    .with_chrome_capabilities(|caps| caps.add_arg("--window-size=1280,800"));
```

The headless binary is only used in headless runs. Visible runs always use regular Chrome. Capability setups apply to
every session, after the runner's own headless or visible arguments.

## Execution Model

`BrowserTests` says which tests run one after another and which at the same time. A group runs its entries either
sequentially (`BrowserTests::sequential()`) or up to a number at once (`BrowserTests::parallel(Parallelism::parallel(4))`,
or `Parallelism::from_env()?.unwrap_or(...)` to read `BROWSER_TEST_PARALLELISM`). Entries are tests (`with`) and nested groups
(`with_group`), so stages, tests that must not overlap, and run-wide checks can all be expressed:

```rust,no_run
# use std::borrow::Cow;
# use browser_test::thirtyfour::WebDriver;
# use browser_test::{async_trait, BrowserTest, BrowserTests, Parallelism};
# use rootcause::Report;
# macro_rules! test {
#     ($name:ident) => {
#         struct $name;
#         #[async_trait]
#         impl BrowserTest for $name {
#             fn name(&self) -> Cow<'_, str> { stringify!($name).into() }
#             async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> { Ok(()) }
#         }
#     };
# }
# test!(Buttons); test!(Tables); test!(CreateUser); test!(DeleteUser); test!(ServerDidNotPanic);
let tests = BrowserTests::sequential()
    .with_group(
        BrowserTests::parallel(Parallelism::from_env()?.unwrap_or(Parallelism::parallel(4)))
            .with(Buttons)
            .with(Tables)
            // These two share server state, so they must not run at the same time.
            .with_group(BrowserTests::sequential().with(CreateUser).with(DeleteUser)),
    )
    .with_group(
        BrowserTests::sequential()
            .named("after all")
            .run_always()
            .with(ServerDidNotPanic),
    );
# Ok::<(), browser_test::InvalidEnvVar>(())
```

Entries start in the order they were added. A test whose browser is already running may start before an earlier test
whose browser is still starting. Only run tests in parallel that can safely share the same application state. The
chromedriver process is shared for the run, so captured driver output can contain interleaved lines from different
sessions.

By default, the runner stops starting tests after the first failure (`FailurePolicy::FailFast`). Tests in groups marked
`run_always()` still run, e.g. checks that must see the whole run. With `FailurePolicy::RunAll`, every test of every
group runs, and all failures are returned as child reports on one aggregate `Report<BrowserTestError>`. A sequential
group only means its tests must not run at the same time: a failing test does not skip the ones after it.

Named groups (`named(...)`) are listed with their wall time in the run report, and their tests' records carry the name.

## Sessions

Every test runs in a fresh `WebDriver` session, so no browser state (cookies, storage, open windows, ...) leaks from one
test into the next. Creating a session starts a new browser, which takes a noticeable share of a short test's time. The
runner therefore keeps fresh sessions ready while tests run: by default, one spare session per test that can run at the
same time. A test usually finds its session ready when its turn comes, and sessions are quit in the background after
their test.

Up to `parallel tests + spare sessions` browsers are open at once. Lower the number of spare sessions on machines with
little memory, or disable them, with `BrowserTestRunner::with_spare_sessions(n)`. In visible runs, the browser windows
of spare sessions open ahead of their tests.

Spare sessions use the runner's element-query wait. A test that overrides `element_query_wait()` with a different value
gets a session created when its turn comes, so it waits for its browser to start.

## Timing and Progress

Every test's timing is logged (`tracing`, `info` level) when it finishes: how long creating its session took and how
long the test waited for it, its body, and the session teardown. At the end of each run, the runner hands a
`BrowserTestRunReport` to every `RunReportConsumer` added with `BrowserTestRunner::with_report_consumer`. It prints or logs
nothing on its own. `StderrSummary`, `StdoutSummary`, and `TracingSummary` print a summary of the report. Any closure
taking a `&BrowserTestRunReport` works as a consumer too:

```rust
use browser_test::{BrowserTestRunner, StderrSummary};

let runner = BrowserTestRunner::new().with_report_consumer(StderrSummary);
```

The summary looks like this:

```text
Browser test run: 39 test(s), 39 passed, 0 failed, in 2m 10.4s
  chromedriver:   started in 95ms, stopped in 1ms
  sessions:       39 created in 9.12s (avg 233ms), tests waited 310ms for them, quit in 1.95s
  test bodies:    2m 09.8s
  slowest tests:
       40.05s  menu_tests (session 240ms, body 40.05s, quit 50ms)
  ...
  slowest steps (by total time):
       52.30s  wait_for_no_selector: 18x, max 3.10s
  ...
```

To see which steps of a test are slow, time them with `StepExt::step`, e.g. inside your page-object helpers. The step
kind (`"goto"`) aggregates across tests. The optional detail is only logged:

```rust,no_run
# use browser_test::thirtyfour::{WebDriver, error::WebDriverResult};
use browser_test::StepExt;

async fn goto(driver: &WebDriver, url: &str) -> WebDriverResult<()> {
    driver.goto(url).step("goto").detail(url).await
}
```

Steps are logged at `debug` level with their duration, steps slower than 2 seconds at `warn` level. Each test's
`BrowserTestRecord::steps` aggregates them per kind, and the summary lists the kinds that took the most time. Each test
body also runs in a `browser_test` tracing span carrying the test name, so all logs of a test can be attributed to it.

A watchdog warns when a test is still running after 30 seconds (and every 30 seconds after that), and when creating
or quitting a session takes longer than 5 seconds. Such slowness often points at an overloaded machine or a test waiting
for something that never happens. Configure or disable the thresholds with `BrowserTestRunner::with_progress_warnings`.

The runner converts test panics into `BrowserTestError::Panic` reports and still shuts down chromedriver after errors
or panics.

## Examples

The repository includes runnable examples:

```sh
cargo run --manifest-path examples/minimal/Cargo.toml
cargo run --manifest-path examples/advanced/Cargo.toml
```

Both open Wikipedia in a visible browser. The advanced example adds a parallel group, timed steps and the run summary,
tracing spans with rootcause span and backtrace collectors, a custom test error type, explicit timeouts and
element-query waits, driver-output capture, and the pause prompt (`BROWSER_TEST_PAUSE=1`).

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

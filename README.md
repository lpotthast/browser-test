# browser-test

[![Crates.io](https://img.shields.io/crates/v/browser-test.svg)](https://crates.io/crates/browser-test)
[![Docs.rs](https://docs.rs/browser-test/badge.svg)](https://docs.rs/browser-test)
[![CI](https://github.com/lpotthast/browser-test/actions/workflows/ci.yml/badge.svg)](https://github.com/lpotthast/browser-test/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.89.0-blue.svg)](https://github.com/lpotthast/browser-test/blob/main/Cargo.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/browser-test.svg)](#license)

`browser-test` is a test runner for browser tests written in async Rust.

You write a test as an async function marked with `#[browser_test]`, or as a type that implements the `BrowserTest`
trait. Each test gets a [`thirtyfour`](https://docs.rs/thirtyfour) `WebDriver` to control the browser, and a context
of your choice, such as your app's base URL. Collect the tests in `BrowserTests` and pass them to a `BrowserTestRunner`,
which handles everything else. It downloads Chrome for Testing, starts chromedriver, creates a browser session for each
test, runs the tests one after another or in parallel, and shuts everything down at the end. When a test fails, the
runner shows the line of test code that failed and the steps the test took before. It also records how long each test
and each browser session took.

The runner does not start your web app. Your test must do that before the tests run, potentially using `cargo run`,
`cargo leptos serve`, Docker Compose, a static fixture page or whatever else fits. It then passes the app's URL, or
whatever else the tests need, to the runner as the context.

For testing ([`leptos`](https://leptos.dev)) apps, I built ([
`leptos-browser-test`](https://crates.io/crates/leptos-browser-test)), which can start and manage a Leptos app under
test for you.

## Contents

- [What It Does](#what-it-does)
- [Getting Started](#getting-started)
- [Writing Tests](#writing-tests)
- [Organizing Tests](#organizing-tests)
- [Configuring the Runner](#configuring-the-runner)
- [Sessions](#sessions)
- [Timing](#timing)
- [Failure Reports](#failure-reports)
- [Examples](#examples)
- [Leptos Projects](#leptos-projects)

## What It Does

- Downloads and caches Chrome for Testing and starts the matching chromedriver, using `chrome-for-testing-manager`.
- Runs tests one after another or in parallel, in named groups that can be nested. By default, it stops at the first
  failure.
- Gives every test a clean browser. New sessions start in the background while earlier tests run. With session reuse,
  a finished test's browser is reset and handed to the next test instead.
- Points each failure at the line of test code that failed, along with the calls that led there and the last steps
  the test took.
- Measures sessions, test bodies and the steps you mark, and can print a summary of the slowest tests and steps after
  each run. It warns when a test or a session takes unusually long.
- Helps with local debugging: visible browser windows, a pause before tests start, and recent chromedriver output
  attached sto errors.

## Getting Started

### Installation

Add `browser-test` to the crate that holds your browser tests. E.g. when implement them as integration tests:

```toml
[dev-dependencies]
browser-test = "0.6"
rootcause = "0.13"
tokio = { version = "1", default-features = false, features = ["macros", "rt-multi-thread"] }
```

Tests return `rootcause` reports as their errors, and Tokio provides the async runtime.

`browser-test` uses `thirtyfour` as its `WebDriver` client and re-exports it. Import it through the re-export, so that
your tests and the runner always use the same version:

```rust
use browser_test::{BrowserTest, BrowserTestRunner, BrowserTests};
use browser_test::thirtyfour::{By, WebDriver, prelude::*};
```

Only add `thirtyfour` as a direct dependency if you need to manage that dependency yourself.

### First Test

This test opens Wikipedia and checks the page title.

```rust,no_run
use std::time::Duration;

use browser_test::thirtyfour::{ChromiumLikeCapabilities, WebDriver};
use browser_test::{
    BrowserTestError, BrowserTestRunner, BrowserTests, Cancellation, Channel, ChromeBinary,
    DriverOutput, ElementQueryWait, FailurePolicy, Pause, ProgressWarnings, SessionReuse,
    StderrSummary, Timeouts, Visibility, browser_test,
};
use rootcause::{Report, report};

struct Context {
    base_url: String,
}

/// The page title names Wikipedia.
#[browser_test]
async fn page_title(driver: &WebDriver, context: &Context) -> Result<(), Report> {
    driver.goto(&context.base_url).await?;

    let title = driver.title().await?;
    if !title.contains("Wikipedia") {
        return Err(report!(
            "unexpected page title: expected it to contain \"Wikipedia\", got {title:?}",
        ));
    }
    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Report<BrowserTestError>> {
    let context = Context {
        base_url: "https://www.wikipedia.org".into(),
    };

    BrowserTestRunner::new(Cancellation::on_shutdown_signals())
        // The browser.
        .with_channel(Channel::Stable)
        .with_visibility(Visibility::Visible)
        .with_headless_chrome_binary(ChromeBinary::ChromeHeadlessShell)
        .with_chrome_for_testing_cache_dir("target/chrome-for-testing")
        .with_chrome_profiles_dir("target/browser-test-profiles")
        .with_chrome_capabilities(|caps| caps.add_arg("--window-size=1280,800"))
        // The sessions the tests run in.
        .with_timeouts(Timeouts::new().with_page_load(Duration::from_secs(10)))
        .with_element_query_wait(ElementQueryWait::new(
            Duration::from_secs(10),
            Duration::from_millis(500),
        ))
        .with_spare_sessions(1)
        .with_session_reuse(SessionReuse::enabled())
        // Failures, slow tests and debugging.
        .with_failure_policy(FailurePolicy::RunAll)
        .with_progress_warnings(ProgressWarnings::default())
        .with_pause(Pause::enabled())
        .with_driver_output(DriverOutput::tail_lines(200))
        .with_failure_report_hooks(true)
        // What happens with the report of each run.
        .with_report_consumer(StderrSummary)
        .run(&context, BrowserTests::sequential().with(PageTitle))
        .await
}
```

`#[browser_test]` turns `page_title` into a unit struct named `PageTitle` that implements `BrowserTest<Context>` (see
[Writing Tests](#writing-tests)). You add it to a `BrowserTests` collection and pass that to the runner together with
the context. `run` returns an error if the runner itself fails, a test returns an error or a test panics. The error is
a `Report<BrowserTestError>` with the details.

To show what can be configured, the example sets every runner option. None of them are required, and
`BrowserTestRunner::new(...)` followed by `run` is enough to get started. Each option is documented on
[`BrowserTestRunner`].

A few things to keep in mind:

- The runner needs a multithreaded Tokio runtime, because the Chrome for Testing manager depends on one. In
  integration tests, use `#[tokio::test(flavor = "multi_thread")]`.
- `BrowserTestRunner::new` takes a `Cancellation`, which decides when a run stops early. A cancelled run shuts down
  chromedriver and its browsers and removes their profiles. A run that is interrupted without being cancelled can
  leave them running. `Cancellation::on_shutdown_signals()` cancels on Ctrl-C or SIGTERM,
  `Cancellation::from_token(token)` cancels when your own token is cancelled, and `Cancellation::disabled()` never
  cancels. The runner also shuts chromedriver down when a run ends with an error or a panic.
- The runner logs through `tracing`. Install a subscriber, for example from `tracing-subscriber`, to see its output.

### Cargo Features

- `rustls` (default): Download Chrome for Testing using `rustls` with the `aws-lc-rs` crypto provider.
- `rustls-no-provider`: Download using `rustls` with the process-default crypto provider. You have to install that
  provider before running tests.
- `native-tls`: Download using the platform's native TLS implementation.
- `component`: Enable `#[derive(Component)]` in the re-exported `thirtyfour`.

The TLS features are passed on to `chrome-for-testing-manager`. Chrome for Testing is downloaded over HTTPS, so one of
them must be enabled, or every download fails. The connection to chromedriver on localhost does not use TLS.

To use the `ring` crypto provider instead of `aws-lc-rs`, enable `rustls-no-provider` and install `ring` as the
process-default provider. No crate in your dependency graph may enable `reqwest/rustls` in that case. A direct
`thirtyfour` dependency with default features does, so turn them off:

```toml
[dev-dependencies]
browser-test = { version = "0.6", default-features = false, features = ["rustls-no-provider"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
# Only if you depend on `thirtyfour` directly.
thirtyfour = { version = "0.37", default-features = false, features = ["reqwest"] }
```

```rust,ignore
// Once per process, before running tests. If a provider is already installed, this returns an error you can ignore.
let _ = rustls::crypto::ring::default_provider().install_default();
```

## Writing Tests

`#[browser_test]` is the easiest way to write a test. The function takes no arguments, the run's context
(`&Context`), or the session's driver and the context (`&WebDriver, &Context`), and returns `Result<(), Report>`. The
generated struct is named after the function in `PascalCase`. The test's name is the module-qualified function name,
which `#[browser_test(name = "...")]` replaces, and the function's doc comments become the test's description. The
[attribute's documentation][browser_test] lists every supported signature.

Implement the [`BrowserTest`] trait yourself for tests that take parameters, wrap other tests, or need their own
timeouts or a browser no other test has used.

## Organizing Tests

[`BrowserTests`] is a group of tests. A group runs its entries one after another (`BrowserTests::sequential()`) or up
to a given number at a time (`BrowserTests::parallel(...)`), and groups can be nested. For example, a sequential group
inside a parallel one keeps a few tests that share server state from overlapping, while the other tests run in
parallel. Only run tests in parallel if they can safely share your application's state.

By default, the runner starts no further tests after the first failure. Groups marked with `run_always()` still run,
which suits checks that need to see the whole run. With [`FailurePolicy::RunAll`][FailurePolicy], every test runs and
all failures are reported together.

To run only some of your tests, select them with a [`TestFilter`], by name or by the [`TestGroup`] you put them in.
`TestFilter::from_env()` reads the selection from `BROWSER_TEST_FILTER` and `BROWSER_TEST_GROUP`. The runner runs every
test it is given, so apply the filter with `BrowserTests::filter`.

## Configuring the Runner

[First Test](#first-test) shows every runner option. Some of them can be read from environment variables,
which makes them easy to switch on for a single run:

```rust,no_run
use browser_test::{BrowserTestRunner, Cancellation, DriverOutput, Pause, Visibility};

let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals())
    .with_visibility(Visibility::from_env()?.unwrap_or_default())
    .with_pause(Pause::from_env()?.unwrap_or_default())
    .with_driver_output(DriverOutput::from_env()?.unwrap_or_default());
# Ok::<(), browser_test::InvalidEnvVar>(())
```

```sh
BROWSER_TEST_VISIBLE=1 BROWSER_TEST_PAUSE=1 BROWSER_TEST_DRIVER_OUTPUT=1 cargo test -- --nocapture
```

`from_env()` returns `None` if the variable is not set, so you choose the default. A value it cannot parse, such as
`BROWSER_TEST_VISIBLE=ture`, is an error instead of being ignored.

| Variable                                | Read by                    | Effect                                        |
|-----------------------------------------|----------------------------|-----------------------------------------------|
| `BROWSER_TEST_VISIBLE`                  | `Visibility::from_env()`   | Show the browser instead of running headless. |
| `BROWSER_TEST_PAUSE`                    | `Pause::from_env()`        | Ask before starting the browser.              |
| `BROWSER_TEST_DRIVER_OUTPUT`            | `DriverOutput::from_env()` | Attach recent chromedriver output to errors.  |
| `BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES` | `DriverOutput::from_env()` | Output lines to keep (default 200).           |
| `BROWSER_TEST_PARALLELISM`              | `Parallelism::from_env()`  | Number of tests to run at the same time.      |
| `BROWSER_TEST_SESSION_REUSE`            | `SessionReuse::from_env()` | Turn session reuse on or off.                 |
| `BROWSER_TEST_FILTER`                   | `TestFilter::from_env()`   | Test name substrings, comma-separated.        |
| `BROWSER_TEST_GROUP`                    | `TestFilter::from_env()`   | Exact test group names, comma-separated.      |

Boolean variables accept `0`/`1`, `false`/`true`, `no`/`yes`, `off`/`on` and `disabled`/`enabled`, regardless of case.
The pause gives you time to look at the app or attach a debugger before the browser starts.

## Sessions

Every test runs in its own `WebDriver` session, so no cookies, storage or open windows carry over from one test to
the next. Starting a browser takes time, so the runner prepares sessions for upcoming tests while earlier tests run.
[`BrowserTestRunner::with_spare_sessions`][with_spare_sessions] sets how many it keeps ready.

### Session Reuse

A run of many short tests spends a lot of its time starting browsers. With [`SessionReuse`] enabled, a finished test's
session is reset and handed to the next test instead. [`SessionReset`] offers two strategies. `NewContext`, the
default, runs every test in an isolated browser context, similar to an incognito window, and removes it afterwards.
`Manual` clears the browser's state item by item and can keep the HTTP cache, so later tests load the app faster.

### Memory and Parallelism

Every test running at the same time needs a browser, and so does every spare session. A browser showing a page with a
mid-sized WASM app uses about 0.5 GB. For example, we measured a suite of 821 tests of a Leptos app on a machine with
32 threads. At a parallelism of 8, the browsers used 4.9 GB on average and the run took 1m 10s. At 16, they used 7.2 GB
and the run took 58s, because the CPU was saturated and pages loaded more slowly. Only raise the parallelism while runs
get noticeably faster, and keep the number of spare sessions low.

What a test loads matters more than the browser itself. An app built in release mode loads and hydrates much faster
and uses less memory in every test.

## Timing

At the end of each run, the runner passes a [`BrowserTestRunReport`] to the consumers you add with
`BrowserTestRunner::with_report_consumer`. `StderrSummary` prints a summary like this:

```text
Browser test run: 39 test(s), 39 passed, 0 failed, in 2m 10.4s
  chromedriver:   started in 95ms, stopped in 1ms
  sessions:       39 created in 9.12s (avg 233ms), tests waited 310ms for them, quit in 1.95s
  test bodies:    2m 09.8s
  slowest tests:
       40.05s  menu_tests (session 240ms, body 40.05s, quit 50ms)
  ...
  slowest steps (by total time):
       52.30s  wait_for_no_selector: 18x (avg 2.91s), max 3.10s
  ...
```

To see which parts of your tests are slow, mark them as steps with [`StepExt::step`][StepExt]. Your page-object
helpers are a good place for this:

```rust,no_run
# use browser_test::thirtyfour::{WebDriver, error::WebDriverResult};
use browser_test::StepExt;

async fn goto(driver: &WebDriver, url: &str) -> WebDriverResult<()> {
    driver.goto(url).step("goto").detail(url).await
}
```

The runner also warns when a test or a session takes unusually long (see [`ProgressWarnings`]).

## Failure Reports

A failing test's report tells you what went wrong, where in your test code it happened, and what the test did just
before:

```text
 ● Browser test 'checkbox' failed.
 ├ Last steps (time since the test started):
 │   +0ms      navigate /atoms/checkbox (355ms)
 │   +364ms    find #value (4ms)
 │   +744ms    wait_for_text "checked" (10.00s)
 │
 ● the text of <span id="value"> did not become "checked" within 10s, it is still "false"
 ╰ Test code:
     tests/pages/element.rs:281  ElementExt::wait_for_text
     tests/checkbox.rs:102       checkbox::selected_state
```

The test code frames are recorded for every error and every panic, without any extra code in your tests. The last
steps are the steps the test marked. The [`failure_report`] module explains the details, including how to combine the
runner's `rootcause` hooks with hooks of your own.

To get the most out of these reports:

- **Return errors with `?`.** Write tests and helpers as `async fn ... -> Result<_, Report>`. Each `?` that converts
  an error records its line.
- **Say what you expected and what you got.** A helper that waits or checks should fail with a message naming the
  element, the expected value and the actual value. The location is added for you, but the values are not.
- **Mark steps** in your page-object helpers, so that "Last steps" shows what the test was doing and for how long.
- **Add context where a loop or a shared helper hides what was going on**, for example with
  `.context_with(|| format!("after pressing {key:?}"))`.
- **Keep debug info.** Test frames need line tables. If your profile sets `debug = false`, use at least
  `debug = "line-tables-only"`.

## Examples

The repository contains two runnable examples:

```sh
cargo run --manifest-path examples/minimal/Cargo.toml
cargo run --manifest-path examples/advanced/Cargo.toml
```

Both open Wikipedia in a visible browser. The advanced example adds a parallel group, timed steps, the run summary,
tracing spans, a custom error type for tests, explicit timeouts, driver output capture and the pause prompt.

## Leptos Projects

If you would rather have the app's lifecycle handled for you, use `leptos-browser-test`. It starts the Leptos test
app, waits until it accepts connections, keeps the app's recent output in case startup fails, and then passes the base
URL to `BrowserTestRunner`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.

[browser_test]: https://docs.rs/browser-test/latest/browser_test/attr.browser_test.html
[`BrowserTest`]: https://docs.rs/browser-test/latest/browser_test/trait.BrowserTest.html
[`BrowserTests`]: https://docs.rs/browser-test/latest/browser_test/struct.BrowserTests.html
[FailurePolicy]: https://docs.rs/browser-test/latest/browser_test/enum.FailurePolicy.html
[`TestFilter`]: https://docs.rs/browser-test/latest/browser_test/struct.TestFilter.html
[`TestGroup`]: https://docs.rs/browser-test/latest/browser_test/struct.TestGroup.html
[`BrowserTestRunner`]: https://docs.rs/browser-test/latest/browser_test/struct.BrowserTestRunner.html
[with_spare_sessions]: https://docs.rs/browser-test/latest/browser_test/struct.BrowserTestRunner.html#method.with_spare_sessions
[`SessionReuse`]: https://docs.rs/browser-test/latest/browser_test/struct.SessionReuse.html
[`SessionReset`]: https://docs.rs/browser-test/latest/browser_test/enum.SessionReset.html
[`BrowserTestRunReport`]: https://docs.rs/browser-test/latest/browser_test/struct.BrowserTestRunReport.html
[StepExt]: https://docs.rs/browser-test/latest/browser_test/trait.StepExt.html#method.step
[`ProgressWarnings`]: https://docs.rs/browser-test/latest/browser_test/struct.ProgressWarnings.html
[`failure_report`]: https://docs.rs/browser-test/latest/browser_test/failure_report/index.html

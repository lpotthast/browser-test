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
one `WebDriver` session per test (fresh, or reset after an earlier test), reports where the time went, and shuts the driver down again.

Use this crate when your project already owns app startup and wants a focused runner for the browser
side of the integration test.

## What It Does

- Manages Chrome for Testing and chromedriver through `chrome-for-testing-manager`.
- Runs async test functions (`#[browser_test]`) and other `BrowserTest` values collected in `BrowserTests`.
- Gives every test a fresh `WebDriver` session, created in the background while earlier tests run, or, with session
  reuse, the reset session of an earlier test (see "Sessions").
- Runs sequentially and fails fast by default.
- Reports failures with the line of test code that failed, its callers, and the test's last steps (see "Failure
  Reports").
- Measures every test (session creation or reset, waiting for it, body, teardown) and reports each run: the slowest tests, the
  slowest steps, and the time spent on sessions, printed as a summary on request. Warns when tests or sessions take
  unusually long.
- Supports bounded parallel runs, run-all failure reporting, visible Chrome, manual pauses,
  `WebDriver` timeouts, element-query wait configuration, Chrome capability customization, and recent
  browser-driver output on failures.

## Installation

Add `browser-test` to the crate that owns your browser integration tests:

```toml
[dev-dependencies]
browser-test = "0.6"
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

## Features

- `rustls` *(default)*: Downloads Chrome for Testing through `rustls` with the `aws-lc-rs` crypto provider.
- `rustls-no-provider`: Downloads through `rustls` with the process-default crypto provider, which you must install
  before running tests.
- `native-tls`: Downloads through the platform's native TLS implementation.
- `component`: Enables `#[derive(Component)]` in the re-exported `thirtyfour`.

The TLS features are forwarded to `chrome-for-testing-manager`. Its release index and downloads are served over HTTPS,
so enable one of them. Without one, every download fails. Talking to `chromedriver` on localhost needs no TLS.

To use the `ring` crypto provider instead of `aws-lc-rs`, select `rustls-no-provider` and install `ring` as the
process-default provider. No crate in your dependency graph may enable `reqwest/rustls`. A direct `thirtyfour`
dependency with default features would, so disable them:

```toml
[dev-dependencies]
browser-test = { version = "0.6", default-features = false, features = ["rustls-no-provider"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
# Only if you depend on `thirtyfour` directly.
thirtyfour = { version = "0.37", default-features = false, features = ["reqwest"] }
```

```rust,ignore
// Once per process, before running tests. Fails harmlessly if a provider is already installed.
let _ = rustls::crypto::ring::default_provider().install_default();
```

## Minimal Test

This example opens Wikipedia. In a real integration test, the shared context is usually your app's
base URL or a small struct with whatever the tests need.

```rust,no_run
use browser_test::thirtyfour::WebDriver;
use browser_test::{
    BrowserTestError, BrowserTestRunner, BrowserTests, Cancellation, Visibility, browser_test,
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
        .with_visibility(Visibility::Visible)
        .run(&context, BrowserTests::sequential().with(PageTitle))
        .await
}
```

`#[browser_test]` turns the function into a unit struct, `PageTitle`, implementing `BrowserTest<Context>` (see "Defining
Tests"). `BrowserTestRunner::run(...)` returns `Report<BrowserTestError>`, so runner failures, test failures,
and panics get useful context. The runner logs through `tracing`. Install a subscriber (e.g. `tracing-subscriber`) to see
its output.

Browser tests must run on a multithreaded Tokio runtime because the Chrome for Testing manager requires it.
Use `#[tokio::test(flavor = "multi_thread")]` for integration tests.

`BrowserTestRunner::new` requires a `Cancellation`, deciding how runs are stopped early. A cancelled run shuts down
`ChromeDriver` and its browsers and removes their profiles, while an interrupted run that is not cancelled can leave them
running. `Cancellation::on_shutdown_signals()` cancels runs on Ctrl-C or SIGTERM, `Cancellation::from_token(token)` once
your application's own token is cancelled, and `Cancellation::disabled()` never.

## Local Debugging

Configure the runner with environment-driven options:

```rust,no_run
use browser_test::{BrowserTestRunner, Cancellation, DriverOutput, Pause, Visibility};

let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals())
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
| `BROWSER_TEST_FILTER`                   | `TestFilter::from_env()`    | Comma-separated test-name substrings (any match).          |
| `BROWSER_TEST_GROUP`                    | `TestFilter::from_env()`    | Comma-separated exact logical group names (any match).    |

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
    BrowserTestRunner, Cancellation, DriverOutput, ElementQueryWait, FailurePolicy, Pause,
    StderrSummary, Timeouts, Visibility,
};

let pause = Pause::from_env()?
    .unwrap_or_default()
    .with_hint("The app runs at http://127.0.0.1:3000");
let pause_enabled = pause.is_enabled();

let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals())
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
use browser_test::{BrowserTestRunner, Cancellation, Channel, ChromeBinary};

let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals())
    .with_channel(Channel::Stable)
    .with_chrome_for_testing_cache_dir("target/chrome-for-testing")
    .with_headless_chrome_binary(ChromeBinary::ChromeHeadlessShell)
    .with_chrome_capabilities(|caps| caps.add_arg("--window-size=1280,800"));
```

The headless binary is only used in headless runs. Visible runs always use regular Chrome. Capability setups apply to
every session, after the runner's own headless or visible arguments.

Every session gets a fresh Chrome profile, removed when the session ends. When a run starts, it also removes the
profiles that killed runs left behind. Profiles are kept in `browser-test-profiles` in the system's temporary directory.
Choose another place with `.with_chrome_profiles_dir(ChromeProfilesDir::new("target/browser-test-profiles"))`.
Capability setups must not set `--user-data-dir`.

## Defining Tests

`#[browser_test]` turns an async function into a test: a unit struct implementing `BrowserTest`, named in `PascalCase`
(`async fn opens_menu` becomes `OpensMenu`), with the function's visibility and doc comments. Register it with
`.with(OpensMenu)`.

```rust
use browser_test::{BrowserTest, BrowserTests, browser_test, thirtyfour::WebDriver};
use rootcause::Report;

struct Page<'a> {
    driver: &'a WebDriver,
    base_url: &'a str,
}

/// The menu opens on click.
#[browser_test]
async fn opens_menu(page: &Page<'_>) -> Result<(), Report> {
    page.driver.goto(format!("{}/menu", page.base_url)).await?;
    Ok(())
}

#[browser_test(name = "menu::closes")]
async fn closes_menu(driver: &WebDriver, base_url: &str) -> Result<(), Report> {
    driver.goto(format!("{base_url}/menu")).await?;
    Ok(())
}

assert!(OpensMenu.name().ends_with("::opens_menu"));
assert_eq!(OpensMenu.description().as_deref(), Some("The menu opens on click."));
assert_eq!(ClosesMenu.name(), "menu::closes");
let tests: BrowserTests<str> = BrowserTests::sequential().with(ClosesMenu);
```

- **Arguments**: none, the run's context (`&Context`), or the session's driver and the context
  (`&WebDriver, &Context`). Contexts can be unsized (`str`) or borrow from the run (`Page<'_>`), e.g. a page object
  that a wrapping test creates for every test.
- **Result**: `Result<(), Report>` or `Result<(), Report<E>>`, also through a type alias. The future must be `Send`.
- **Name**: the module-qualified function name (`my_tests::menu::opens_menu`), which filters match.
  `#[browser_test(name = "menu::closes")]` sets another one.
- **Description**: `BrowserTest::description()` returns the doc comments. Filters don't match it.

The function becomes the test's body and keeps its other attributes. Type and const parameters, `&mut` arguments, and
non-async functions are rejected at the declaration:

```compile_fail
use browser_test::browser_test;
use rootcause::Report;

#[browser_test]
fn synchronous(_context: &()) -> Result<(), Report> { Ok(()) }
```

```compile_fail
use browser_test::browser_test;
use rootcause::Report;

#[browser_test]
async fn mutable_context(_context: &mut ()) -> Result<(), Report> { Ok(()) }
```

An `async fn` taking `&Context` is a `BrowserTest` without the attribute too, named by its Rust path;
`BrowserTest::named` gives any test another name. Implement `BrowserTest` yourself for tests with session settings of
their own (timeouts, element-query wait, a fresh session), tests with parameters, and wrappers around other tests.

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

### Logical Groups and Selection

A `TestGroup` makes tests selectable by a name of its own, e.g. the tests of one component, wherever their code lives.
Added with `with_test_group`, its tests run like tests added one by one with `with`: in the enclosing `BrowserTests`'s
parallelism limit, in order. A logical group never changes how its tests run. Test records carry its name, and wall
times are measured for named `BrowserTests`.

```rust
use browser_test::{BrowserTests, Parallelism, TestFilter, TestGroup, browser_test};
use rootcause::Report;

#[browser_test]
async fn presses(_base_url: &str) -> Result<(), Report> { Ok(()) }
#[browser_test]
async fn presses_with_keyboard(_base_url: &str) -> Result<(), Report> { Ok(()) }

let tests: BrowserTests<str> = BrowserTests::parallel(Parallelism::parallel(8))
    .with_test_group(TestGroup::new("button").with(Presses).with(PressesWithKeyboard))
    .filter(&TestFilter::from_env()?);
# Ok::<(), browser_test::InvalidEnvVar>(())
```

`TestFilter` selects tests by name substring and by exact logical group name:

- `BROWSER_TEST_FILTER=press,keyboard` (`TestFilter::new().with_name_containing("press")`) selects tests whose names
  contain either substring.
- `BROWSER_TEST_GROUP=button` (`TestFilter::new().with_group("button")`) selects the tests of that group. Ungrouped tests
  are then left out.
- Combined, a test must match both. Without either, every test is selected.

Values are case-sensitive, whitespace around comma-separated entries is trimmed, and empty entries are ignored. An
unknown group or unmatched name selects no tests. `with_name_substrings_from_env_var` and `with_groups_from_env_var`
read variables of your choice, and `BrowserTests::filter_tests` and `filter_groups` take predicates for custom selection.

Selection is explicit: the runner runs every test it is given, so call `filter`. Filtering keeps how nested groups run.
Add checks that must always run **after** filtering: `run_always` exempts tests from fail-fast, not from filters.

## Sessions

Every test runs in a fresh `WebDriver` session, so no browser state (cookies, storage, open windows, ...) leaks from one
test into the next. Creating a session starts a new browser, which takes a noticeable share of a short test's time. The
runner therefore keeps fresh sessions ready while tests run: by default, one spare session per test that can run at the
same time. A test usually finds its session ready when its turn comes, and sessions are quit in the background after
their test.

Up to `parallel tests + spare sessions` browsers are open at once. Lower the number of spare sessions on machines with
little memory, or disable them, with `BrowserTestRunner::with_spare_sessions(n)`. With session reuse (below), the default
is one spare session per eight parallel tests: a returned session only needs a reset, not a new browser.

Visible runs keep no spare sessions by default, because a spare session's browser window would open on top of the
window of the running test. Each test then waits for its browser to start. Set `with_spare_sessions(n)` explicitly to
create spare sessions in visible runs as well.

Spare sessions use the runner's element-query wait. A test that overrides `element_query_wait()` with a different value
gets a session created when its turn comes, so it waits for its browser to start.

### Session Reuse

A run of many short tests spends much of its time starting browsers. With `SessionReuse::enabled()`, sessions return to
the runner's pool after their test: reset, they run further tests as long as upcoming tests need them, and quit once the
pool has enough. A run then creates about as many sessions as tests run at the same time, plus spares, however many tests
it has:

```rust
use browser_test::{BrowserTestRunner, Cancellation, SessionReuse};

let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals())
    .with_session_reuse(SessionReuse::from_env()?.unwrap_or(SessionReuse::enabled()));
# Ok::<(), browser_test::InvalidEnvVar>(())
```

How a session is reset is its `SessionReset`. Both strategies give the next test a browser without the state of the tests
before; run a suite with each to cross-check that its tests don't depend on the strategy.

`SessionReset::NewContext` (the default) runs every test in a tab of a user context of its own (`WebDriver` `BiDi`'s
isolated browser profiles, like incognito windows), and the reset removes it: its tabs and windows (with their history,
page state and CDP overrides), cookies, storage of every origin, caches and permissions, and the renderer processes of its
pages. Complete by construction, whatever a test changed, but nothing is kept: every test downloads and compiles the app's
scripts and WebAssembly again. The session's first tab stays open on `about:blank`, so a test sees two windows; windows a
test opens with `WebDriver`'s New Window command (they open in the browser's default context) are closed. Grant
permissions through CDP for the test's context: `Browser.grantPermissions` with the `browserContextId` of the current tab
(`Target.getTargetInfo`).

`SessionReset::manual([CachedData::Http])` runs every test of a session in the session's one tab and clears what tests can
change item by item, keeping only the cached data listed:

```rust
use browser_test::{BrowserTestRunner, CachedData, Cancellation, SessionReset, SessionReuse};

let runner = BrowserTestRunner::new(Cancellation::on_shutdown_signals()).with_session_reuse(
    SessionReuse::enabled().with_reset(SessionReset::manual([CachedData::Http])),
);
```

It closes other windows, navigates to an empty page in a new renderer process (a `data:` URL: the test's page and its
process go, with the caches in it) and clears the history, clears cookies and the storage
(`localStorage`, `sessionStorage`, `IndexedDB`, cache storage, service workers, file systems) of every origin the tab
showed, resets CDP permissions and emulation overrides (device metrics, user
agent, geolocation, media, timezone, locale, touch, CPU throttling, idle state, focus emulation, background color, script
execution), and clears the HTTP cache unless `CachedData::Http` is kept. Keeping it is safe while the served files don't
change during a run (content-hashed names, as in production): the next test loads the app's scripts and WebAssembly from
the cache, with the code V8 compiled for them. State the list doesn't cover (e.g. Shared Storage, Storage Buckets, other
CDP domains' settings) survives into the next test; use `NewContext` for tests that change it.

Reusable sessions start Chrome without its back/forward cache (`--disable-features=BackForwardCache`, merged into a
`--disable-features` of your own), so both strategies behave alike within a test (going back loads a page again), and no
cached page keeps a renderer process (100 to 250 MB) alive in a reused browser. `SessionReuse::with_back_forward_cache(true)`
keeps it, e.g. for tests of pages restored from it.

A test that needs a browser no test ran in (e.g. one measuring a first page load, with empty caches) returns `true` from
`BrowserTest::fresh_session`: the pool gives it a session no test ran in, creating one if none is ready. Afterwards its
session returns to the pool like any other. A session whose reset fails (e.g. its browser crashed) quits.
`SessionReuse::with_max_tests_per_session(n)` bounds how many tests one browser runs. `BROWSER_TEST_SESSION_REUSE=0`
(read by `SessionReuse::from_env`) turns reuse off for a run, e.g. to compare timings.

The run report counts created and reset sessions separately (see "Timing and Progress").

### Memory and Parallelism

Every test that runs at the same time is a browser, and every spare session another one. Their memory adds up fast: a
browser running a page with a mid-size WASM app takes about 0.5 GB (renderer processes, the network service, the
browser and GPU processes). Measured with 821 tests of a Leptos app on 32 threads, at parallelism 8 the browsers took
4.9 GB on average and the run 1m 10s; at 16 they took 7.2 GB and the run 58s, as the CPU was saturated and pages loaded
slower. Raise the parallelism only while runs get noticeably faster, and keep spares low.

What a test loads matters more than the browser: an app built for release loads and hydrates much faster and needs
less memory in every test (with Leptos: `leptos-browser-test`'s `BuildProfile::Release`).

## Timing and Progress

Every test's timing is logged (`tracing`, `info` level) when it finishes: how long creating (or resetting) its session
took and how long the test waited for it, its body, and the session teardown. At the end of each run, the runner hands a
`BrowserTestRunReport` to every `RunReportConsumer` added with `BrowserTestRunner::with_report_consumer`. It prints or logs
nothing on its own. `StderrSummary`, `StdoutSummary`, and `TracingSummary` print a summary of the report. Any closure
taking a `&BrowserTestRunReport` works as a consumer too:

```rust
use browser_test::{BrowserTestRunner, Cancellation, StderrSummary};

let runner =
    BrowserTestRunner::new(Cancellation::on_shutdown_signals()).with_report_consumer(StderrSummary);
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
       52.30s  wait_for_no_selector: 18x (avg 2.91s), max 3.10s
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

A watchdog warns when a test is still running after 30 seconds (and every 30 seconds after that), and when creating,
resetting or quitting a session takes longer than 5 seconds. Such slowness often points at an overloaded machine or a test waiting
for something that never happens. Configure or disable the thresholds with `BrowserTestRunner::with_progress_warnings`.

The runner still shuts down chromedriver after errors or panics.

## Failure Reports

A failing test's report says what went wrong, at which line of your test code, and what the test did before. Without
anything in your test code:

```text
 ● Browser test 'checkbox' failed.
 ├ Last steps (time since the test started):
 │   +0ms      navigate /atoms/checkbox (355ms)
 │   +364ms    find #value (4ms)
 │   +744ms    wait_for_text "checked" (10.00s)
 │
 ● the text of <span id="value"> did not become "checked" within 10s; it is "false"
 ╰ Test code:
     tests/pages/element.rs:281  ElementExt::wait_for_text
     tests/checkbox.rs:102       checkbox::selected_state
```

- **Where**: every error gets the frames of your test code that led to it ("Test code"), innermost first, ending at the
  test's `run`. An error from a helper (a lookup, a wait in a page object) points at the test line that called it. A
  panic (a failed `assert!`/`assertr` assertion, an `unwrap`, an index out of bounds) shows where it panicked and its
  frames; a panic raised inside a dependency (e.g. an assertion library's own code) is marked "outside the test code",
  and the frames show the test line that called it. Test code is the code of the package whose tests run (`CARGO_MANIFEST_DIR`); dependencies are left out.
- **When**: the test's last steps ("Last steps"), with how far into the test each started and how long it took. Steps
  are the futures you mark with `StepExt::step` (see above).
- **What**: the error and the context added on its way up. `thirtyfour` errors show their `WebDriver` message, without
  chromedriver's native stack trace; messages print without quotes and escapes.

The runner installs the [`rootcause`](https://docs.rs/rootcause) hooks behind this with its first run. `rootcause`
takes one set of hooks per process, so if your application installs hooks of its own, add browser-test's to them. The
runner then leaves the installation to you:

```rust,no_run
use browser_test::failure_report;
use rootcause::hooks::Hooks;

failure_report::hooks(Hooks::new())
    // ... your own hooks ...
    .install()
    .expect("hooks are installed once");
```

`BrowserTestRunner::with_failure_report_hooks(false)` goes without the hooks.

### Getting the most out of failure reports

- **Return errors with `?`.** Write test code and helpers as `async fn ... -> Result<_, Report>` and propagate with
  `?`: an error converted by `?` is located at that line, and nothing is lost on the way up. Assertions may panic
  (`assert!`, `assertr`); the runner locates panics as well.
- **Say what was expected and what was seen.** A helper that waits or checks should fail with a message naming the
  element, the expected value and the value it ended with (`rootcause::bail!("... did not become {expected:?}; it is
  {actual:?}")`). The location is added for you; the values are not.
- **Mark steps.** Wrap navigations, lookups and waits (best inside your page-object helpers) and, if you like, each case
  of a test in `.step(kind).detail(..)`. They make up "Last steps", so a report shows what the test was doing and for how
  long, e.g. a lookup that waited 10 seconds.
- **Add context where a loop or a shared helper hides what was going on**:
  `.context_with(|| format!("after pressing {key:?}"))` adds a line above the error, `.attach(..)` adds any value that
  implements `Display` (a URL, a form's values) below it.
- **Keep debug info.** Test frames need line tables, which the default `dev` and `test` profiles have. With
  `debug = false` in your profile, keep at least `debug = "line-tables-only"`.

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

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.8.0] - 2026-10-27

### Changed

- **Breaking:** Updated `thirtyfour` to version `0.38`.
- Updated `chrome-for-testing-manager` to version `0.15`.
- Updated `assertr` to version `0.9`.

## [0.7.0] - 2026-10-13

### Added

- Time limits: a test whose body runs longer than its time limit fails with the new `BrowserTestError::TimedOut` and
  `TestOutcome::TimedOut` (counted as failed, shown as `[timed out]` in the run summary). Tests set a limit of their own
  with the new `TestSettings::with_time_limit`, or with the new `time_limit` argument of `#[browser_test]`, e.g.
  `#[browser_test(time_limit = Duration::from_secs(10))]`. The others get the runner's, a last resort set with
  `BrowserTestRunner::with_time_limit(limit)`. The limit covers the test's body only, not preparing its session.
- Frozen pages: once a test exceeds its time limit, the runner checks the pages and cross-origin frames of its session
  through Chrome's `DevTools` endpoint. A frozen one (its main thread never yields, e.g. to an endless chain of
  microtasks) blocks every further `WebDriver` command of its session, even quitting it, so it is closed. The test's
  report lists the closed pages and frames. A dialog (`alert`, `confirm`, `prompt`, `beforeunload`) blocks its page
  as well, but not the session: the report names it with its text instead, and it is left open. A session whose test
  timed out is never reused.
- `BrowserTestRecord::path`: the names of the named `BrowserTests` a test runs in, joined with ` / `.
- `BrowserTestError::PrepareSession`, for a test whose session could not be created, ended before the test started,
  or did not accept the test's timeouts.

### Changed

- **Breaking:** Tests time out after 10 minutes by default, so a test that runs longer now fails. Raise the limit with
  `BrowserTestRunner::with_time_limit`, or give the test a limit of its own. Before, a hanging test could keep its run
  waiting forever.
- **Breaking:** `SessionSettings` became `TestSettings`, and `BrowserTest::session_settings` became
  `BrowserTest::settings`. It also holds the test's time limit, so a wrapper that forwards `settings` forwards every
  setting of the wrapped test.
- **Breaking:** `BrowserTestRecord::group` is only the name of the test's `TestGroup`, the name
  `TestFilter::with_group` selects it by. Before, it also held the names of the named `BrowserTests` the test runs in,
  which are in the new `path` now.
- **Breaking:** A failed run always returns `BrowserTestError::RunTests`, with the failures as child reports. Before, a
  `FailurePolicy::FailFast` run in which only one test could run at a time returned the failed test's report directly.
- **Breaking:** A test whose session can't be prepared fails with the new `BrowserTestError::PrepareSession` rather
  than `BrowserTestError::RunTest`, so that such failures can be told apart from failures of the test. Its outcome is
  still `TestOutcome::Failed`.
- **Breaking:** `SessionPreparation`, `FailurePolicy` and `SessionReset` are `#[non_exhaustive]`, so that they can gain
  variants without a breaking change.
- Resetting a reused session (see `SessionReuse`) takes at most 30 seconds. A session whose reset takes longer has its
  frozen pages closed, like a test that timed out, and quits instead of running further tests.
- The message of `BrowserTestError::RunTests` reads "One or more browser tests failed, panicked or timed out (N
  failed)."

### Fixed

- A failure to quit the session of a test that failed is no longer lost: it is a child of the test's report, as it
  already failed a test that passed. Quitting a session that ran no test, or that was released after a reset, logs a
  warning when it fails.
- After a `FailurePolicy::FailFast` stop, tests in groups marked `run_always` still get spare sessions and reset ones,
  instead of each waiting for a new browser.
- After a `FailurePolicy::FailFast` stop, a test waiting for its session gives up right away, instead of waiting for a
  browser that then quits unused. A test with an element query wait of its own still waits for its session to be
  created.
- A test that was running when the run was cancelled no longer stays on the progress board, which went on warning that
  it was still running.
- `browser-test-macros` now requires `quote` 1.0.35 and `proc-macro2` 1.0.91, the oldest versions its dependencies build
  with.

## [0.6.1] - 2026-10-12

### Changed

- Updated `assertr` to version `0.8`.
- Clearer API documentation.

## [0.6.0] - 2026-10-11

### Added

- Failure reports (module `failure_report`): every error of a failing test carries the frames of the test code that led
  to it (`TestCodeFrames`, innermost first, also for errors created in helpers), panics carry their location (marked
  "outside the test code" when a dependency raised them) and frames, and a failing test's report lists its last steps
  with their timing (`RecentSteps`). `thirtyfour` errors print their `WebDriver` message without `ChromeDriver`'s
  native stack trace, and messages print without quotes and escapes, also where a report is printed with `Debug`. The
  first run with tests installs the `rootcause` hooks behind this. Applications with hooks of their own add them with
  `failure_report::hooks`, and runners then leave the installation to them.
  `BrowserTestRunner::with_failure_report_hooks(false)` leaves the `rootcause` hooks out. Either way, the first run with
  tests installs a process-wide panic hook that locates panics. It calls the previously installed hook, and a panic
  hook set later replaces it. See the README's "Failure Reports".
- `BrowserTestRunner::with_chrome_profiles_dir(path)` sets where runs keep the Chrome profiles of their sessions.
  Defaults to `"browser-test-profiles-<uid>"` (Unix, with the user id) or `"browser-test-profiles"` in the system's
  temporary directory. The directory must not be a symlink and, on Unix, must belong to the user running the tests and
  be accessible only by them (mode `700`). Otherwise, the run fails with `BrowserTestError::CreateChromeProfiles`.
- Cancellation of runs, e.g. on Ctrl-C. A cancelled run starts no further tests, cancels running ones, and shuts down
  `ChromeDriver` and its browsers, then `run` returns the new `BrowserTestError::Cancelled`. Without it, an interrupted
  run can leave `ChromeDriver` and its browsers running. See the breaking change below. `CancellationToken` is
  re-exported.
- TLS backend features `rustls` (default), `rustls-no-provider` and `native-tls`, forwarded to
  `chrome-for-testing-manager`. They choose how Chrome for Testing is downloaded, e.g. with the `ring` crypto provider
  instead of `aws-lc-rs`. See the README.
- Feature `component` enables `#[derive(Component)]` in the re-exported `thirtyfour`.
- Session reuse: `BrowserTestRunner::with_session_reuse(SessionReuse::enabled())` returns sessions to the pool after
  their test. Once reset, a session runs further tests as long as upcoming tests need it, instead of a browser starting
  per test. The pool keeps as many sessions as tests run at the same time, plus spares (by default one per eight
  parallel tests), and quits the others. A session whose reset fails quits. Two resets (`SessionReset`) can cross-check
  each other. `NewContext` (the default) runs every test in a tab of its own WebDriver BiDi user context and then
  removes that context with its tabs, cookies, storage, caches, permissions and renderer processes. It quits a session
  whose test left state in the browser's default context, e.g. by opening a new window or navigating the first tab.
  The `bidi` feature of `thirtyfour` is enabled for it. `SessionReset::manual([CachedData::Http])` resets the
  session's single tab item by item and clears its cached data except the listed kinds, e.g. the HTTP cache with V8's
  compiled code. Its documentation lists the few kinds of state that survive. Both resets release pressed keys and
  buttons and restore the window rect and timeouts. With session reuse, every session runs without Chrome's
  back/forward cache unless `SessionReuse::with_back_forward_cache(true)`. `SessionSettings::with_fresh_session(true)`
  asks the pool for a session no test ran in, `SessionReuse::with_max_tests_per_session` bounds how many tests a
  browser runs, and `SessionReuse::from_env` reads `BROWSER_TEST_SESSION_REUSE`. Disabled by default. See the README's
  "Session Reuse".
- The run report counts session resets (`BrowserTestRunReport::session_resets`, `session_reset_time`), and the summary
  shows the average duration of the step kinds it lists. `BrowserTestRunReport::released_session_teardown` measures
  the quits of sessions that ended without a test: spares no test took, and sessions the pool released after their
  test or a failed reset. `session_teardown_time` includes it. `BrowserTestRunReport::not_started` counts the tests a
  fail-fast stop or a cancellation kept from starting, and the summary counts them as well.
- `#[browser_test]`, re-exported from the new `browser-test-macros` crate, turns an async function into a test: a unit
  struct implementing `BrowserTest`, named in PascalCase (`async fn opens_menu` becomes `OpensMenu`), registered with
  `.with(OpensMenu)`. Its name defaults to the module-qualified function name and can be set with `name = "..."`. Its
  doc comments are its `BrowserTest::description`. Functions take no arguments, a context reference (also borrowed, e.g.
  `&Page<'_>`), or a driver and a context reference, and return `Result<(), Report<E>>`.
- `BrowserTest::description`: what a test checks, in prose.
- Logical `TestGroup`s, added with `BrowserTests::with_test_group`: tests selectable by a group name, wherever their
  code lives. They run like tests added one by one.
- `TestFilter` selects tests by name substring (`with_name_containing`) and logical group (`with_group`), applied with
  `BrowserTests::filter`. `TestFilter::from_env` reads `BROWSER_TEST_FILTER` and `BROWSER_TEST_GROUP`.
  `BrowserTests::filter_tests` and `filter_groups` select by predicate.
- Async functions taking a context reference implement `BrowserTest`. `BrowserTest::named` gives any test another name,
  keeping its settings. The trait's default name is the Rust type name.
- `SessionSettings`, returned by the new `BrowserTest::session_settings`, holds what a test needs of its session:
  timeouts, element-query wait, and whether it needs a fresh session.
- `TestOutcome::is_passed`, `is_failed` and `is_cancelled`, `BrowserTestRunReport::passed`, `failed` and `cancelled`.
- `Parallelism::max_parallel_tests` is public.

### Changed

- A test's start ("Executing browser test: ...") and a passed test's timing are logged at `debug` level instead of
  `info`, so a run logs little at `info`.
- Visible runs keep no spare sessions by default, as a spare session's browser window would open on top of the window
  of the running test. `BrowserTestRunner::with_spare_sessions(n)` sets them explicitly.
- A step that is dropped before it finishes, e.g. by a timeout around it or a panic unwinding through it, is recorded
  with its time until then and logged as "ended unfinished". Such steps were not recorded before.
- **Breaking:** `SessionTiming::creation` became `SessionTiming::preparation`, a `SessionPreparation`: `Created` with
  the creation time, or `Reset` with the reset time of a reused session. `SessionTiming` no longer implements
  `Default`. `BrowserTestRecord::teardown` is `None` for a test whose session ran further tests.
- **Breaking:** `BrowserTestRunner::new` requires a `Cancellation`, so that every user decides how runs are cancelled.
  `Cancellation::on_shutdown_signals()` cancels runs on SIGINT (Ctrl-C) or SIGTERM, or Ctrl-C on Windows. The process
  starts listening for them with its first run that has tests, once and on a thread of its own, so any number of
  runners and Tokio runtimes can use it. From then on, a first signal only cancels runs and no longer ends the process.
  A second signal exits the process right away. If the process cannot listen for the signals, `run` fails with the new
  `BrowserTestError::ListenForShutdownSignals`. `Cancellation::from_token(CancellationToken)` cancels runs once your
  own token is cancelled. `Cancellation::disabled()` keeps the previous behavior. Replace `BrowserTestRunner::new()`
  with `BrowserTestRunner::new(Cancellation::on_shutdown_signals())`.
- **Breaking:** `BrowserTestRunner` no longer implements `Default`, as it has no default `Cancellation`.
- **Breaking:** `TestOutcome` is `#[non_exhaustive]` and has a new variant, `Cancelled`: a test that was running when
  its run was cancelled is recorded with it.
- **Breaking:** `BrowserTest::timeouts` and `element_query_wait` were replaced by `BrowserTest::session_settings`,
  returning a `SessionSettings`. A wrapper around another test must forward `name`, `description` and
  `session_settings` (`BrowserTest::named` does), and settings added later reach the wrapped test without changes to
  the wrapper. Replace `fn timeouts(&self) -> Option<Timeouts> { Some(t) }` with
  `fn session_settings(&self) -> SessionSettings { SessionSettings::new().with_timeouts(t) }`.
- **Breaking:** A test's timeouts override the runner's individually. Timeouts the test leaves unset keep the runner's
  values, where they previously stayed at `ChromeDriver`'s defaults.
- **Breaking:** `Timeouts` and `ProgressWarnings` are built with `with_*` methods like every other setting.
  `Timeouts::builder().script_timeout(d).build()` became `Timeouts::new().with_script(d)` (also `with_page_load` and
  `with_implicit_wait`), and the getters lost their `_timeout` suffix.
  `ProgressWarnings::builder().test_running(d).build()` became
  `ProgressWarnings::default().with_test_running(Some(d))`, where `None` disables a warning.
- **Breaking:** `ElementQueryWait::new` returns the wait itself and panics on a zero poll interval, like
  `tokio::time::interval`. Drop the `.expect(..)` or `?` after it.
- **Breaking:** `Parallelism::parallel(0)` and `DriverOutput::tail_lines(0)` panic instead of meaning sequential or
  disabled. Use `Parallelism::sequential()` and `DriverOutput::disabled()`. `BROWSER_TEST_PARALLELISM` and
  `BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES` must be positive.
- **Breaking:** `BrowserTests::with_group` was renamed to `with_nested`, so that "group" no longer names both nested
  `BrowserTests` and logical `TestGroup`s.
- **Breaking:** `BrowserTestError::FlushPausePrompt` was renamed to `WritePausePrompt`.
- **Breaking:** Updated `chrome-for-testing-manager` to 0.14.
- **Breaking:** The re-exported `thirtyfour` no longer has all of its default features enabled, which
  `chrome-for-testing-manager` 0.13 enabled. Without `component`, enable this crate's `component` feature to keep using
  `#[derive(Component)]`. Without `manager`, `thirtyfour::manager` and `WebDriver::managed` are gone.
- **Breaking:** A `--user-data-dir` set through `with_chrome_capabilities` makes sessions fail to start. The runner
  chooses every session's profile directory itself.
- The dependencies of `browser-test` are declared without their default features and enable only what this crate
  needs. Only the selected TLS feature enables `reqwest`'s TLS backend now. `thirtyfour` no longer enables
  `reqwest/rustls`.

### Fixed

- Browser runs no longer leak Chrome profiles into the temporary directory. `ChromeDriver` removes the profile it
  creates for a session (`org.chromium.Chromium.scoped_dir.*`, tens of megabytes each) only when the session is quit
  cleanly. Killed runs could leak them until the disk filled up. The runner now manages profiles itself: every session
  gets a fresh profile in the profiles directory, removed when the session ends, and a starting run removes the
  profiles that killed runs left behind. A run whose profiles cannot be set up fails with the new
  `BrowserTestError::CreateChromeProfiles`. Sessions still start with a focused page: on a profile it did not create
  itself, `ChromeDriver` starts the page without focus, so the runner brings every new session's page to the front (CDP
  `Page.bringToFront`).
- At most `parallel tests + spare sessions` browsers are open at once, as documented. Sessions still quitting in the
  background did not count towards this bound, so more browsers could be open.
- With `FailurePolicy::FailFast`, a test that was waiting for its browser session when another test failed started
  once its session arrived. It no longer starts.
- The run summary prints durations just below a whole number of minutes correctly, e.g. 119.96 s as `2m 00.0s` instead
  of `1m 60.0s`, and 59.996 s as `1m 00.0s` instead of `60.00s`. Durations below a second are rounded instead of
  truncated.
- A panic in a Chrome capability setup, or in a hand-written `BrowserTest::run` before it returns its future, fails that
  session or test. Before, it unwound through the whole run, skipping the shutdown of `ChromeDriver`.
- Driver output capture no longer captures a line twice that was printed while the capture started.
- Driver output capture no longer reserves memory for all of its tail lines up front. A large
  `BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES` or `DriverOutput::tail_lines` reserved gigabytes, and `usize::MAX` panicked.

### Removed

- **Breaking:** `TimeoutsBuilder`, `ProgressWarningsBuilder`, `ElementQueryWaitError`, and the `typed-builder`
  dependency.

## [0.5.0] - 2026-10-06

This release simplifies the configuration API: every setting has exactly one way to express it, and the runner only
does what it is told. See "Changed" for the renames.

### Added

- User-defined scheduling: `BrowserTests` is a nestable group that runs its entries sequentially
  (`BrowserTests::sequential()`) or up to a number at once (`BrowserTests::parallel(Parallelism)`). Entries are tests
  (`with`) and nested groups (`with_group`), so stages, tests that must not run at the same time, and run-wide checks
  can be expressed in one run. `named(...)` names a group. The run report lists the wall time of named groups
  (`BrowserTestRunReport::groups`, `GroupRecord`), and every `BrowserTestRecord::group` names the test's group.
  `run_always()` makes a group's tests run even after a fail-fast stop. A sequential group only means its tests must
  not overlap: with `FailurePolicy::RunAll`, a failing test does not skip the ones after it.
- Sessions are created ahead of time: while tests run, the runner keeps fresh `WebDriver` sessions ready, so a starting
  test usually finds its session ready instead of waiting for a browser to start. Sessions are quit in the background
  after their test. Every test still gets its own fresh session. Configure the number of spare sessions with
  `BrowserTestRunner::with_spare_sessions` (default: one per test that can run at the same time, `0` creates sessions
  only on demand). Visible runs default to `0`, so that no spare browser window opens on top of the running test.
- Run reports: every test's session creation, the time it waited for its session, its body, and its session teardown
  are measured and logged when it finishes. At the end of a run, the runner hands a `BrowserTestRunReport` to every
  `RunReportConsumer` added with `BrowserTestRunner::with_report_consumer`. `StderrSummary`, `StdoutSummary`, and
  `TracingSummary` print a summary (counts, session time, slowest tests, slowest steps). Closures can process the
  report in any other way. Without a consumer, nothing is printed.
- `StepExt::step` times a future as a step of a test (e.g. a navigation or a wait in a page-object helper):
  `driver.goto(url).step("goto").detail(url).await`. Steps are logged, slow steps are warned about, and their durations
  are aggregated per kind into the test's record and the run summary. Test bodies run in a `browser_test` tracing span
  with the test's name.
- Progress warnings: a watchdog warns when a test runs long or session creation or teardown is slow. Configure with
  `BrowserTestRunner::with_progress_warnings` and `ProgressWarnings`.
- `Parallelism::from_env()` / `from_env_var(var)` read the number of parallel tests from `BROWSER_TEST_PARALLELISM`
  or another variable.
- `InvalidEnvVar`, the error of every `from_env` / `from_env_var` constructor for a value that cannot be interpreted.

### Changed

- **Breaking:** `BrowserTests` decides how tests run: `BrowserTests::new()` and `push` are replaced by
  `BrowserTests::sequential()` / `BrowserTests::parallel(Parallelism)` and the chaining `with` / `with_group`, and
  `BrowserTestRunner::with_test_parallelism` is removed. `BrowserTestRecord::slot` is replaced by
  `BrowserTestRecord::group`.
- **Breaking:** Configuration types have short, consistent names, and every runner option takes its type directly (no
  `impl Into`):
    - `BrowserTestVisibility` → `Visibility`, `PauseConfig` → `Pause`, `DriverOutputConfig` → `DriverOutput`,
      `BrowserTimeouts` → `Timeouts`, `ElementQueryWaitConfig` → `ElementQueryWait` (and
      `ElementQueryWaitConfigError` → `ElementQueryWaitError`), `BrowserTestParallelism` → `Parallelism`,
      `BrowserTestFailurePolicy` → `FailurePolicy`.
- **Breaking:** The `Resolved*` types (`ResolvedBrowserTestVisibility`, `ResolvedPauseConfig`,
  `ResolvedDriverOutputConfig`) are removed. `from_env()` and `from_env_var(var)` read the environment when they are
  called and return `Result<Option<Self>, InvalidEnvVar>`: `None` for an unset or empty variable, so the caller picks
  the default (`Visibility::from_env()?.unwrap_or_default()`), and an error for a value that cannot be interpreted.
  Unrecognized boolean values (e.g. `ture`) were previously treated as disabled.
- **Breaking:** Settings carrying data are built through constructors only: `Parallelism::sequential()` /
  `parallel(n)`, `DriverOutput::disabled()` / `tail_lines(n)`, `Pause::disabled()` / `enabled()`.
  `ElementQueryWait::new(timeout, interval)` validates the interval and is its only constructor (its builder,
  `try_new`, and the unvalidated `new` are removed).
- **Breaking:** The pause hint moved from `BrowserTestRunner::with_hint` to `Pause::with_hint`.
- **Breaking:** `BrowserTestRunner::run` is the only way to run tests. Reports are handed to report consumers. There is
  no `run_with_report` or `BrowserTestRunOutcome`.
- **Breaking:** `BrowserTestError` is `#[non_exhaustive]`. `StartWebdriver` / `TerminateWebdriver` are renamed to
  `LaunchChromeForTesting` / `ShutDownChromeForTesting`, matching what they report.
- **Breaking:** Updated `chrome-for-testing-manager` to 0.13. The re-exported `Channel` now comes from
  `chrome-for-testing` 0.5, which knows the Linux ARM64 platform. Chrome for Testing's version list now includes that
  platform, which `chrome-for-testing-manager` 0.12 fails to parse, so earlier versions of this crate can no longer
  start the browser.
- Captured browser-driver output now includes the driver's startup output. Output lines are numbered by the capture.
  If launching fails, the driver's recent output is part of the error.
- With `FailurePolicy::FailFast`, parallel slots stop starting tests as soon as a test body failed, not only once the
  failed test's session was quit.

### Removed

- **Breaking:** The deprecated `BrowserTestRunner::with_webdriver_timeouts`,
  `BrowserTestRunner::with_browser_driver_output`,
  and `BrowserDriverOutputConfig`.

## [0.4.0] - 2026-06-17

### Added

- Added `ResolvedBrowserTestVisibility` and `BrowserTestVisibility::resolve`. BrowserTestVisibility stays spec oriented.
  Environment-backed visibility can now be resolved by users of this crate.
- Added `ResolvedPauseConfig` and `PauseConfig::resolve`. PauseConfig stays spec oriented. Environment-backed pause
  settings can now be resolved by users of this crate.
- Added `ResolvedDriverOutputConfig` and `DriverOutputConfig::resolve`. DriverOutputConfig stays spec oriented.
  Environment-backed browser-driver output settings can now be resolved by users of this crate.

### Changed

- **Breaking:** `PauseConfig::from_env` and `PauseConfig::from_env_var` now store an environment-backed spec and read
  the
  environment when the config is resolved. Previously they read the environment immediately.
- **Breaking:** `PauseConfig::is_enabled` now resolves the config before returning the enabled state and is no longer a
  `const fn`.

## [0.3.0] - 2026-06-17

### Added

- Added `BrowserTestRunner::with_chrome_for_testing_cache_dir` to override the Chrome-for-Testing download cache
  directory.
- Added `BrowserTestRunner::with_headless_chrome_binary` and re-exported `ChromeBinary` so headless runs can use Chrome
  Headless Shell while visible runs continue to force regular Chrome.

### Changed

- **Breaking:** Updated `chrome-for-testing-manager` to version `0.12`.
- **Breaking:** Updated `rootcause` to version `0.13`.

## [0.2.1] - 2026-05-11

### Fixed

- Updated `chrome-for-testing-manager` to version `0.11`. The runner now creates each `WebDriver` session through the
  new `Chromedriver::session` builder API and supplies the configured element poller (built from
  `ElementQueryWaitConfig`) at session-creation time. Previously the runner created a default session and then
  attached the poller via `WebDriver::clone_with_config`, producing a sibling `WebDriver` that shared the session's
  quit guard. When that sibling was dropped at the end of our closure, before the original was `.quit().await`-ed, by
  `chrome-for-testing-manager`, `thirtyfour` emitted a "WebDriver was not quit properly" warning and ran the cleanup on
  a blocking OS thread per test. With session creation now carrying the poller, no clone is needed anymore.

### Changed

- Replaced `ElementQueryWaitConfig::into_thirtyfour_webdriver_config` with
  `ElementQueryWaitConfig::into_thirtyfour_poller`.

### Added

- Added a GitHub Actions CI workflow (`.github/workflows/ci.yml`) running `fmt`, `check`, `clippy`, unit tests,
  integration tests, `build`, `doc`, and an MSRV check.
- Added crates.io, docs.rs, CI status, MSRV, and license badges to the README.
- Added the `## License` section to the README.

## [0.2.0] - 2026-05-09

### Changed

- **Breaking:** Updated `thirtyfour` to version `0.37`
- Updated `chrome-for-testing-manager` to version `0.10`
- Updated `assertr` to version `0.6`

## [0.1.0] - 2026-04-17

### Added

- Added the `BrowserTest` and `BrowserTests` traits for defining async, browser-driven integration tests.
- Added `BrowserTestRunner` for running browser tests with Chrome for Testing.
- Added `BrowserTimeouts` and `ElementQueryWaitConfig` for runner-level and per-test timeout configuration.
- Added runner configuration for Chrome channel, visibility, parallelism, failure policy, and Chrome capabilities.
- Added `PauseConfig` and runner pause/hint configuration.
- Added `DriverOutputConfig` for browser-driver stdout/stderr diagnostics on failures.
- Added `BrowserTestError` contexts and panic reporting.
- Added environment-variable controls for visibility, pauses, and browser-driver output diagnostics.
- Added re-exports for `async_trait::async_trait`, `chrome_for_testing_manager::Channel` and the `thirtyfour` crate.

[Unreleased]: https://github.com/lpotthast/browser-test/compare/v0.8.0...HEAD

[0.8.0]: https://github.com/lpotthast/browser-test/compare/v0.7.0...v0.8.0

[0.7.0]: https://github.com/lpotthast/browser-test/compare/v0.6.1...v0.7.0

[0.6.1]: https://github.com/lpotthast/browser-test/compare/v0.6.0...v0.6.1

[0.6.0]: https://github.com/lpotthast/browser-test/compare/v0.5.0...v0.6.0

[0.5.0]: https://github.com/lpotthast/browser-test/compare/v0.4.0...v0.5.0

[0.4.0]: https://github.com/lpotthast/browser-test/compare/v0.3.0...v0.4.0

[0.3.0]: https://github.com/lpotthast/browser-test/compare/v0.2.1...v0.3.0

[0.2.1]: https://github.com/lpotthast/browser-test/compare/v0.2.0...v0.2.1

[0.2.0]: https://github.com/lpotthast/browser-test/compare/v0.1.0...v0.2.0

[0.1.0]: https://github.com/lpotthast/browser-test/releases/tag/v0.1.0

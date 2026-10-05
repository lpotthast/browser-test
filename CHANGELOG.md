# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-10-06

This release simplifies the configuration API: every setting has exactly one way to express it, and the runner only
does what it is told. See "Changed" for the renames.

### Added

- User-defined scheduling: `BrowserTests` is a nestable group that runs its entries sequentially
  (`BrowserTests::sequential()`) or up to a number at once (`BrowserTests::parallel(Parallelism)`). Entries are tests
  (`with`) and nested groups (`with_group`), so stages, tests that must not run at the same time, and run-wide checks
  can be expressed in one run. `named(...)` names a group; the run report lists the wall time of named groups
  (`BrowserTestRunReport::groups`, `GroupRecord`), and every `BrowserTestRecord::group` names the test's group.
  `run_always()` makes a group's tests run even after a fail-fast stop. A sequential group only means its tests must
  not overlap: with `FailurePolicy::RunAll`, a failing test does not skip the ones after it.
- Sessions are created ahead of time: while tests run, the runner keeps fresh `WebDriver` sessions ready, so a starting
  test usually finds its session ready instead of waiting for a browser to start. Sessions are quit in the background
  after their test. Every test still gets its own fresh session. Configure the number of spare sessions with
  `BrowserTestRunner::with_spare_sessions` (default: one per test that can run at the same time; `0` creates sessions
  only on demand).
- Run reports: every test's session creation, the time it waited for its session, its body, and its session teardown
  are measured and logged when it finishes. At the end of a run, the runner hands a `BrowserTestRunReport` to every
  `RunReportConsumer` added with `BrowserTestRunner::with_report_consumer`. `StderrSummary`, `StdoutSummary`, and
  `TracingSummary` print a summary (counts, session time, slowest tests, slowest steps); closures can process the
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
- **Breaking:** Configuration types have short, consistent names, and every runner option takes its type directly
  (no `impl Into`):
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
- **Breaking:** `BrowserTestRunner::run` is the only way to run tests. Reports are handed to report consumers; there is
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

- **Breaking:** The deprecated `BrowserTestRunner::with_webdriver_timeouts`, `BrowserTestRunner::with_browser_driver_output`,
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

- **Breaking:** `PauseConfig::from_env` and `PauseConfig::from_env_var` now store an environment-backed spec and read the
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

[Unreleased]: https://github.com/lpotthast/browser-test/compare/v0.5.0...HEAD

[0.5.0]: https://github.com/lpotthast/browser-test/compare/v0.4.0...v0.5.0

[0.4.0]: https://github.com/lpotthast/browser-test/compare/v0.3.0...v0.4.0

[0.3.0]: https://github.com/lpotthast/browser-test/compare/v0.2.1...v0.3.0

[0.2.1]: https://github.com/lpotthast/browser-test/compare/v0.2.0...v0.2.1

[0.2.0]: https://github.com/lpotthast/browser-test/compare/v0.1.0...v0.2.0

[0.1.0]: https://github.com/lpotthast/browser-test/releases/tag/v0.1.0

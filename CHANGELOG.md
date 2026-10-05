# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Session reuse: tests share `WebDriver` sessions, which the runner resets between tests (new blank tab with all other
  windows closed, cookies and storage cleared, window rect and timeouts restored). Tests opt out with the new
  `BrowserTest::session` method returning `SessionRequirement::Fresh`. Only tests with equal effective timeouts and
  element query waits share a session, and a session is never reused after a failed or panicking test. Configure it
  with `BrowserTestRunner::with_session_reuse` (`bool` or `SessionReuse`, incl. `SessionReuse::from_env()` reading
  `BROWSER_TEST_SESSION_REUSE`). Add app-specific resets with `BrowserTestRunner::with_session_reset` and the
  `SessionReset` trait.
- Timing: every test's session acquisition (created or reused), body, and teardown are measured and logged when it
  finishes. A summary (counts, session time, slowest tests, slowest steps) is printed at the end of every run, see
  `BrowserTestRunner::with_run_summary` and `RunSummary`. `BrowserTestRunner::run_with_report` returns the data as a
  `BrowserTestRunReport`.
- `browser_test::step`: times a step of a test (e.g. a navigation or a wait in a page-object helper), logs it, warns
  when it is slow, and aggregates its duration per kind into the test's record and the run summary. Test bodies run
  in a `browser_test` tracing span with the test's name.
- Progress warnings: a watchdog warns when a test runs long or session creation, reset, or teardown is slow.
  Configure with `BrowserTestRunner::with_progress_warnings` and `ProgressWarnings`.

### Changed

- **Behavior:** Tests no longer get a fresh `WebDriver` session each by default; see "Session reuse" above. Use
  `with_session_reuse(false)` for the previous behavior.
- With `BrowserTestFailurePolicy::FailFast`, parallel slots stop starting tests as soon as a test body failed, not only
  once the failed test's session was quit.
- The `thirtyfour` dependency now enables its `cdp` feature (used to clear cookies and storage).

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

[Unreleased]: https://github.com/lpotthast/browser-test/compare/v0.4.0...HEAD

[0.4.0]: https://github.com/lpotthast/browser-test/compare/v0.3.0...v0.4.0

[0.3.0]: https://github.com/lpotthast/browser-test/compare/v0.2.1...v0.3.0

[0.2.1]: https://github.com/lpotthast/browser-test/compare/v0.2.0...v0.2.1

[0.2.0]: https://github.com/lpotthast/browser-test/compare/v0.1.0...v0.2.0

[0.1.0]: https://github.com/lpotthast/browser-test/releases/tag/v0.1.0

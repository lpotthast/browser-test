# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

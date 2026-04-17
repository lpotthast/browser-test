# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

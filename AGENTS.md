# Repository Guidelines

## Project Structure & Module Organization

This repository is a Rust 2024 library crate for async browser-driven integration test helpers. Public exports live in
`src/lib.rs`; implementation is split across modules such as `runner`, `test_case`, `pause`, `scheduler`, `execution`,
`driver_output`, `wait`, `env`, `timeout`, and `error`.

Integration tests live in `tests/`, with `tests/browser_runner.rs` covering public runner behavior. Runnable examples
are separate crates under `examples/minimal/` and `examples/advanced/`. Package metadata and dependency versions are in
`Cargo.toml` and `Cargo.lock`. Do not edit generated build artifacts under `target/`.

## Build, Test, and Development Commands

- `cargo check` verifies the crate quickly.
- `cargo test` runs unit, doc, and integration tests.
- `cargo test --doc` runs documentation examples only.
- `cargo fmt --check` checks standard Rust formatting; run `cargo fmt` to apply fixes.
- `cargo clippy --all-targets --all-features -- -D warnings` runs strict linting.
- `cargo doc --no-deps --document-private-items` builds local API documentation.
- `just verify` runs the repository validation sequence: check, clippy, test, build, and doc.

Browser-driven runs need Chrome for Testing and chromedriver support from `chrome-for-testing-manager`, and may need
network access for browser downloads.

## Coding Style & Naming Conventions

Use Rust 2024 idioms and preserve compatibility with `rust-version = "1.89.0"`. Follow `rustfmt` defaults. Use
`snake_case` for functions, modules, variables, and test names; `CamelCase` for types, traits, and enum variants; and
`SCREAMING_SNAKE_CASE` for constants.

Keep public APIs small and explicit. Use `rootcause` context for fallible paths. Avoid adding new Tokio runtime features
unless the change requires them.

## Testing Guidelines

Place public API integration tests in `tests/*.rs` and unit tests near private helpers in `#[cfg(test)]` modules. Name
tests after behavior, for example `env_flag_enabled_treats_false_as_disabled`.

Browser tests that call `BrowserTestRunner::run(...)` should use `#[tokio::test(flavor = "multi_thread")]`. Make
environment-sensitive behavior explicit; relevant flags include `BROWSER_TEST_VISIBLE`, `BROWSER_TEST_PAUSE`,
`BROWSER_TEST_DRIVER_OUTPUT`, and `BROWSER_TEST_DRIVER_OUTPUT_TAIL_LINES`.

## Commit & Pull Request Guidelines

This checkout has no Git commits, so no repository-specific commit convention can be inferred. Use concise, imperative
messages such as `Add pause prompt configuration` or `Document browser runner cleanup`.

Pull requests should describe the behavioral change, list validation commands run, and mention browser or environment
assumptions. Link related issues when available. Include screenshots only when changes affect visible browser behavior
in downstream examples or tests.

## Agent-Specific Instructions

Keep changes scoped to source, tests, manifests, examples, and documentation unless broader repository updates are
explicitly requested. Do not edit files under `target/`.

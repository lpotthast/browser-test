# Lists all available commands.
list:
    just --list

# Install tools required by other recipes and for maintenance work, profiling and more...
install-tools:
    cargo +stable install --locked cargo-audit
    cargo +stable install --locked cargo-edit
    cargo +stable install --locked cargo-expand
    cargo +stable install --locked cargo-hack
    cargo +stable install --locked cargo-minimal-versions
    cargo +stable install --locked cargo-msrv
    cargo +stable install --locked cargo-semver-checks
    cargo +stable install --locked cargo-sort
    cargo +stable install --locked cargo-upgrades
    cargo +stable install --locked cargo-whatfeatures

# Find the minimum supported rust version.
msrv:
    cargo msrv find --path browser-test

# Check if the current dependency version bounds are sufficient.
minimal-versions:
    cargo minimal-versions check --workspace --direct

# Check whether current changes require a breaking release.
semver-checks:
    cargo semver-checks -p browser-test

# Format all code, including the examples, which are not part of the workspace.
fmt:
    cargo fmt --all
    cargo fmt --all --manifest-path examples/minimal/Cargo.toml
    cargo fmt --all --manifest-path examples/advanced/Cargo.toml

# Check that `fmt` would not change any code.
fmt-check:
    cargo fmt --all -- --check
    cargo fmt --all --manifest-path examples/minimal/Cargo.toml -- --check
    cargo fmt --all --manifest-path examples/advanced/Cargo.toml -- --check

# Check the code, including the examples.
check:
    cargo check --workspace --all-targets
    cargo check --workspace --all-targets --all-features
    cargo check --manifest-path examples/minimal/Cargo.toml --all-targets
    cargo check --manifest-path examples/advanced/Cargo.toml --all-targets

# Lint the code, including the examples.
clippy:
    cargo clippy --workspace -- -D warnings
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo clippy --manifest-path examples/minimal/Cargo.toml --all-targets -- -D warnings
    cargo clippy --manifest-path examples/advanced/Cargo.toml --all-targets -- -D warnings

# Run all tests, including the browser-tests launching Chrome for Testing. `--no-capture` shows tracing output.
test:
    cargo test --workspace -- --no-capture

# Build all crates.
build:
    cargo build --workspace

# Build the crate documentation.
build-docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# Build and open the crate documentation for review.
open-docs:
    cargo doc -p browser-test --all-features --no-deps --open

# Run security auditing.
audit:
    cargo audit --deny warnings

# Update all deps, sort all Cargo.toml deps and format all code, including the examples.
tidy:
    cargo update
    cargo update --manifest-path examples/minimal/Cargo.toml
    cargo update --manifest-path examples/advanced/Cargo.toml
    cargo sort --workspace
    cargo sort examples/minimal
    cargo sort examples/advanced
    just fmt

# Run the full non-mutating validation suite.
verify:
    just fmt-check
    just check
    just clippy
    just test
    just build
    just build-docs
    just audit
    just semver-checks

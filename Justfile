# Lists all available commands.
list:
  just --list

# Install dependencies for maintenance work, profiling and more...
install-tools:
  cargo +stable install --locked cargo-hack
  cargo +stable install --locked cargo-minimal-versions
  cargo +stable install --locked cargo-msrv
  cargo +stable install --locked cargo-expand
  cargo +stable install --locked cargo-whatfeatures
  cargo +stable install --locked cargo-upgrades
  cargo +stable install --locked cargo-edit
  cargo +stable install --locked cargo-msrv

# Find the minimum supported rust version
msrv:
    cargo msrv find

# Check if the current dependency version bounds are sufficient.
minimal-versions:
    cargo minimal-versions check --workspace --direct

# Run the full validation suite: check, clippy, test, build, doc
verify:
  cargo check
  cargo clippy -- -D warnings
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test
  cargo build
  cargo doc


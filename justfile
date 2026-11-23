# List recipes
default:
    @just --list

# Compile a debug build
build:
    cargo build

# Compile an optimized release build
release:
    cargo build --release

# Run the app; args pass straight through (e.g. `just run --once --json`)
run *args:
    cargo run -- {{args}}

# Run all tests
test:
    cargo test --all-targets

# Check formatting and run clippy with warnings as errors
lint:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings

# Format the code
fmt:
    cargo fmt

# Lint + test: run before every commit
check: lint test

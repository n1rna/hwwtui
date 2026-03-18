# hwwtui — hardware wallet emulator TUI

default:
    @just --list

# Build in debug mode
build:
    cargo build

# Build in release mode
build-release:
    cargo build --release

# Run the TUI (debug build)
run *ARGS:
    cargo run -p hwwtui -- {{ARGS}}

# Run the TUI (release build)
run-release *ARGS:
    cargo run --release -p hwwtui -- {{ARGS}}

# Run all tests
test:
    cargo test

# Run tests with output
test-verbose:
    cargo test -- --nocapture

# Run clippy lints
lint:
    cargo clippy --all-targets -- -D warnings

# Check formatting
fmt-check:
    cargo fmt --check

# Format code
fmt:
    cargo fmt

# Full CI check (fmt + clippy + test)
ci: fmt-check lint test

# Grant current user access to /dev/uhid (needed for virtual HID devices)
setup-uhid:
    sudo setfacl -m u:$USER:rw /dev/uhid
    @echo "UHID access granted for $USER"

# Tail the log file (run in a separate terminal)
logs:
    tail -f /tmp/hwwtui.log

# Clean build artifacts
clean:
    cargo clean

# Show bundle storage location and contents
bundles:
    @echo "Bundle storage: ~/.hwwtui/bundles/"
    @ls -la ~/.hwwtui/bundles/ 2>/dev/null || echo "(no bundles downloaded yet)"

# Remove all downloaded bundles
bundles-clean:
    rm -rf ~/.hwwtui/bundles/
    @echo "All bundles removed"

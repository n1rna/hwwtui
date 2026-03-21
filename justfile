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

# Install udev rules for UHID + hardware wallet HID access (one-time setup)
setup-udev:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Installing udev rules..."
    sudo cp udev/99-hwwtui.rules /etc/udev/rules.d/
    sudo udevadm control --reload-rules
    sudo udevadm trigger
    # /dev/uhid already exists, udev trigger won't re-apply rules to it.
    # Set permissions directly.
    sudo chmod 0660 /dev/uhid
    sudo chgrp plugdev /dev/uhid
    # Ensure current user is in the plugdev group.
    if ! id -nG "$USER" | grep -qw plugdev; then
        echo "Adding $USER to plugdev group..."
        sudo usermod -aG plugdev "$USER"
        echo "NOTE: Log out and back in for the group change to take effect."
        echo "      (or run: newgrp plugdev)"
    fi
    echo ""
    echo "Done. /dev/uhid and hardware wallet HID devices are now accessible."

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

# Test a bundle build in Docker (e.g., just bundle-test trezor)
# For trezor/bitbox02/coldcard/specter: builds in isolated Docker container.
# For ledger/jade: runs on host (they need Docker themselves).
bundle-test wallet:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p out
    if [ "{{wallet}}" = "ledger" ] || [ "{{wallet}}" = "jade" ]; then
        echo "==> Building {{wallet}} on host (requires Docker)..."
        ./scripts/build/{{wallet}}-local.sh
    else
        echo "==> Building {{wallet}} in Docker container..."
        docker build -f scripts/docker/Dockerfile.{{wallet}} -t hwwtui-test-{{wallet}} .
        docker run --rm -v "$(pwd)/out:/out" hwwtui-test-{{wallet}} \
            bash -c './scripts/build/{{wallet}}.sh && cp hwwtui-{{wallet}}-*.tar.gz /out/'
    fi
    echo "Output in out/"
    ls -lh out/hwwtui-{{wallet}}-*.tar.gz 2>/dev/null

# Install a locally-built bundle into ~/.hwwtui/bundles/ for testing
bundle-install wallet:
    #!/usr/bin/env bash
    set -euo pipefail
    TARBALL="out/hwwtui-{{wallet}}-linux-x86_64.tar.gz"
    if [ ! -f "${TARBALL}" ]; then
        echo "Bundle not found: ${TARBALL}"
        echo "Run 'just bundle-test {{wallet}}' first."
        exit 1
    fi
    DEST="$HOME/.hwwtui/bundles/{{wallet}}"
    rm -rf "${DEST}"
    mkdir -p "${DEST}"
    tar xzf "${TARBALL}" --strip-components=1 -C "${DEST}"
    # Write a manifest.json so hwwtui recognizes it
    SIZE=$(du -sb "${DEST}" | cut -f1)
    BINARY=$(cd "${DEST}" && find . -maxdepth 1 -type f -executable | head -1 | sed 's|^\./||')
    cat > "${DEST}/manifest.json" <<EOF
    {
      "wallet_type": "{{wallet}}",
      "version": "dev",
      "platform": "linux-x86_64",
      "installed_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
      "size_bytes": ${SIZE},
      "emulator_binary": "${BINARY:-unknown}",
      "firmware_dir": null,
      "build_info": null
    }
    EOF
    echo "Installed {{wallet}} bundle to ${DEST}"
    ls -lh "${DEST}"

# Build, install, and list all available bundles
bundle-install-all: (bundle-install "trezor") (bundle-install "bitbox02") (bundle-install "coldcard") (bundle-install "specter") (bundle-install "ledger") (bundle-install "jade")
    @just bundles

# Development Guide

## Prerequisites

- **Rust** (stable toolchain, plus `nightly-2025-06-01` for Trezor builds)
- **Docker** (for Ledger/Jade emulators and isolated bundle test builds)
- **just** (task runner): `cargo install just`
- **System packages** (Ubuntu/Debian):
  ```bash
  sudo apt-get install -y libusb-1.0-0-dev libudev-dev pkg-config libclang-dev
  ```

## Building

```bash
# Debug build (all crates)
just build

# Release build
just build-release

# Full CI check: format + clippy + tests
just ci
```

### Individual steps

```bash
just fmt-check    # Check formatting
just fmt          # Auto-format
just lint         # Clippy with -D warnings
just test         # Run all tests
just test-verbose # Tests with stdout
```

## Running

```bash
# Debug build
just run

# Release build
just run-release

# With custom Trezor firmware path (bypasses bundle)
TREZOR_FIRMWARE_PATH=/path/to/trezor-firmware/core just run

# Tail logs in another terminal
just logs
# Logs are written to /tmp/hwwtui.log
```

## Bundle System

### Downloading bundles (in-app)

Press `d` on any wallet tab to download its firmware bundle from GitHub Releases. The download progress is shown in real-time. Bundles are stored in `~/.hwwtui/bundles/{wallet}/`.

### Testing bundle builds locally

```bash
# Build a bundle in an isolated Docker container
just bundle-test trezor
just bundle-test bitbox02
just bundle-test coldcard
just bundle-test specter

# Ledger and Jade run on the host (they need Docker themselves)
just bundle-test ledger
just bundle-test jade
```

Output tarballs land in `out/`.

### Installing bundles locally

```bash
# Install a locally-built bundle (from out/)
just bundle-install trezor

# Install all
just bundle-install-all

# View installed bundles
just bundles

# Remove all bundles
just bundles-clean
```

`bundle-install` extracts the tarball into `~/.hwwtui/bundles/{wallet}/` and writes a `manifest.json` so the app recognizes it.

### Bundle storage layout

```
~/.hwwtui/bundles/
├── trezor/
│   ├── manifest.json         # Written by bundler or bundle-install
│   ├── trezor-emu-core       # Emulator binary
│   ├── lib/                  # Shared libraries
│   ├── src/                  # Python modules
│   └── ...
├── bitbox02/
│   ├── manifest.json
│   └── bitbox02-simulator
└── ...
```

### manifest.json schema

```json
{
  "wallet_type": "trezor",
  "version": "v2.8.9",
  "platform": "linux-x86_64",
  "installed_at": "2026-03-19T00:00:00Z",
  "size_bytes": 42000000,
  "emulator_binary": "trezor-emu-core",
  "firmware_dir": "src",
  "build_info": null
}
```

## CI Pipelines

### `ci.yaml`

Runs on push to main/master and on PRs:
1. Format check (`cargo fmt --check`)
2. Clippy (`cargo clippy --workspace --all-targets -- -D warnings`)
3. Tests (`cargo test --workspace`)
4. Release build (`cargo build --release --workspace`)

System deps: `libusb-1.0-0-dev libudev-dev pkg-config libclang-dev`

### `build-bundles.yaml`

Triggered by:
- Git tags matching `v*`
- Manual `workflow_dispatch` with optional `version` and `wallets` inputs

Builds bundles for all six wallets (or a subset if `wallets` input is specified). Each wallet has its own job with the appropriate firmware repo checked out. On success, creates a GitHub Release with all tarballs.

| Wallet | Firmware Repo | Timeout | Special Deps |
|--------|--------------|---------|-------------|
| Trezor | `trezor/trezor-firmware` @ `core/v2.8.9` | 45 min | nightly-2025-06-01, python deps, SDL2 |
| BitBox02 | `BitBoxSwiss/bitbox02-firmware` | 45 min | cmake, ninja, cbindgen, prost-build-proto |
| Coldcard | `Coldcard/firmware` | 45 min | autoconf, automake, libtool, libltdl-dev |
| Specter | `cryptoadvance/specter-diy` | 45 min | libsdl2-dev, gcc-arm-none-eabi |
| Ledger | `LedgerHQ/app-bitcoin-new` | 60 min | Docker |
| Jade | `Blockstream/Jade` | 90 min | Docker |

### Selective builds

To build only specific wallets via workflow_dispatch, set the `wallets` input to a comma-separated list:

```
wallets: "trezor,bitbox02"
```

## Key Environment Variables

| Variable | Used By | Default | Description |
|---|---|---|---|
| `HWWTUI_GITHUB_REPO` | bundler | `n1rna/hwwtui` | GitHub repo for bundle downloads |
| `TREZOR_FIRMWARE_PATH` | config | (none) | Local `trezor-firmware/core/` path (bypasses bundle) |
| `TREZOR_PROFILE_DIR` | config | `/tmp/hwwtui-trezor` | Emulator flash state directory |
| `TREZOR_PORT` | config | `21324` | UDP port for Trezor emulator |
| `BUNDLE_VERSION` | build scripts | `dev` | Version string for bundle-info.json |
| `FIRMWARE_DIR` | build scripts | varies | Path to checked-out firmware repo |
| `RUST_LOG` | tracing | (none) | Controls log verbosity (e.g. `debug`) |

## UHID Access

For the UHID bridge (currently only implemented for Trezor, and disabled):

```bash
# Grant current user access to /dev/uhid
just setup-uhid
# Equivalent to: sudo setfacl -m u:$USER:rw /dev/uhid
```

Or add a persistent udev rule:
```
KERNEL=="uhid", MODE="0660", GROUP="plugdev"
```

## Adding a New Wallet

Step-by-step guide for adding support for a new hardware wallet emulator:

### 1. Define the wallet type

Add a variant to `WalletType` in `crates/emulators/src/lib.rs`:
```rust
pub enum WalletType {
    // ... existing variants
    NewWallet,
}
```

Update `Display` impl, `wallet_dir_name()` in `crates/bundler/src/storage.rs`, and `WALLET_PREFIXES` in `crates/bundler/src/download.rs`.

### 2. Add device kind

Add a variant to `DeviceKind` in `crates/hwwtui/src/config.rs`. Update `Display` impl and `device_kind_to_wallet_type()` in `crates/hwwtui/src/app.rs`. Add it to the `all_kinds` array in `Config::from_env_or_defaults()`.

### 3. Wire up emulator construction in app.rs

In `App::start_selected()`, add a `DeviceKind::NewWallet` match arm that:
- Looks up the bundle binary via `bundle_manager.emulator_binary_path()`
- Constructs a `GenericEmulator` with the right `TransportConfig`, args, and env vars
- Sets `pane.transport_label`

### 4. Add emulator binary candidates

In `find_emulator_binary()` in `crates/bundler/src/lib.rs`, add known binary names for the new wallet type.

### 5. Create build script

Add `scripts/build/newwallet.sh` following the pattern of `trezor.sh` or `bitbox02.sh`:
- Clone/build the firmware
- Copy binary + runtime deps into a bundle directory
- Write `bundle-info.json`
- Create the tarball

### 6. Create Dockerfile (optional)

Add `scripts/docker/Dockerfile.newwallet` for isolated Docker builds, following `Dockerfile.trezor`.

### 7. Add CI job

Add a `build-newwallet` job in `.github/workflows/build-bundles.yaml` and include it in the `release` job's `needs` array.

### 8. Add protocol decoder (optional)

If the wallet has a protocol you want to decode for the method log, add a module in `crates/protocol/src/`.

### 9. Test

```bash
just bundle-test newwallet
just bundle-install newwallet
just run
# Switch to the new wallet tab with Tab, press s to start
```

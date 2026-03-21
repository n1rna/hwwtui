# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

hwwtui is a terminal UI (TUI) for managing hardware wallet emulators. It supports six wallets: Trezor, BitBox02, Coldcard, Specter DIY, Ledger, and Jade. Built with Rust using ratatui/crossterm for the TUI, tokio for async, and a workspace of five crates.

## Commands

```bash
just build            # Debug build
just run              # Run TUI (debug)
just run-release      # Run TUI (release)
just test             # Run all tests (cargo test --workspace)
just lint             # Clippy (cargo clippy --workspace --all-targets)
just fmt              # Format (cargo fmt)
just ci               # Full CI check: fmt + lint + test
just logs             # Tail /tmp/hwwtui.log
```

Single crate test: `cargo test -p <crate-name>` (crate names: hwwtui, emulators, bridge, protocol, bundler).

System dependencies for building: `libusb-1.0-0-dev libudev-dev pkg-config libclang-dev`.

## Architecture

### Crate dependency graph

```
hwwtui (binary, TUI + event loop)
  ├── emulators    (Emulator trait, process spawning)
  ├── bridge       (UHID virtual HID relay)
  │   ├── emulators
  │   └── protocol
  ├── protocol     (wire protocol decoders)
  └── bundler      (GitHub Release download + ~/.hwwtui/bundles/ storage)
      └── emulators
```

### Key types and data flow

- **`App`** (`crates/hwwtui/src/app.rs`) — Top-level state. Holds a `Vec<DevicePane>` (one per wallet), an action queue, and a shared `BundleManager`. The main loop ticks at 100ms.
- **`DevicePane`** — Per-wallet state: emulator instance, debug link, captured logs, screen content.
- **`Emulator` trait** (`crates/emulators/src/lib.rs`) — Async lifecycle (start/stop/health_check). Two implementations: `TrezorEmulator` and `GenericEmulator` (all other wallets).
- **`Bridge` trait** (`crates/bridge/src/lib.rs`) — UHID relay producing `InterceptedMessage` streams. Currently only `TrezorBridge` exists; UHID support is infrastructure-ready but disabled in the TUI.
- **`BundleManager`** (`crates/bundler/src/lib.rs`) — Facade over `BundleStorage` + `GithubDownloader`. Bundles live at `~/.hwwtui/bundles/{wallet}/manifest.json`.

### Event loop (crates/hwwtui/src/main.rs)

Each 100ms tick: draw frame → poll bridge → drain firmware logs → update download progress → poll debug-link screen (Trezor, ~500ms throttle) → handle input events → process queued actions.

### Rendering

`crates/hwwtui/src/ui.rs` — All ratatui rendering. Tab-per-wallet layout with log pane and optional screen preview (Trezor debug link).

### Configuration

Environment variables defined in `crates/hwwtui/src/config.rs`:
- `HWWTUI_GITHUB_REPO` (default: `n1rna/hwwtui`) — repo for bundle downloads
- `TREZOR_FIRMWARE_PATH` — local trezor-firmware/core path (bypasses bundle)
- `TREZOR_PROFILE_DIR` (default: `/tmp/hwwtui-trezor`) — emulator state directory
- `TREZOR_PORT` (default: `21324`) — UDP port

### Bundle build system

`scripts/build/{wallet}.sh` — per-wallet build scripts. `scripts/docker/Dockerfile.{wallet}` — isolated build environments. `just bundle-test {wallet}` builds in Docker; `just bundle-install {wallet}` installs locally.

## Wallet status

Trezor is the most complete (debug link screen reading, button control). BitBox02/Coldcard/Specter can start emulators. Ledger/Jade are untested (QEMU-based, Docker builds). Non-Trezor wallets need UHID bridge work for desktop wallet integration.

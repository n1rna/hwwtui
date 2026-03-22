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
just setup-udev       # Install udev rules for UHID + hidraw (one-time)
```

Single crate test: `cargo test -p <crate-name>` (crate names: hwwtui, emulators, bridge, protocol, bundler).

Integration tests (require bundles + /dev/uhid): `cargo test -p bridge --test e2e -- --ignored --test-threads=1`

BitBox02 E2E (full UHID bridge flow): `cargo test -p bridge --test bb02_repro -- --ignored`

Coldcard E2E: `cargo test -p bridge --test coldcard_e2e -- --ignored`

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
  ├── bundler      (GitHub Release download + ~/.hwwtui/bundles/ storage)
  │   └── emulators
  └── bitbox-api   (BitBox02 simulator initialization)
```

### Key types and data flow

- **`App`** (`crates/hwwtui/src/app.rs`) — Top-level state. Holds a `Vec<DevicePane>` (one per wallet), an action queue, and a shared `BundleManager`. The main loop ticks at 100ms.
- **`DevicePane`** — Per-wallet state: emulator instance, bridge, debug link, captured logs, screen content.
- **`Emulator` trait** (`crates/emulators/src/lib.rs`) — Async lifecycle (start/stop/health_check). Two implementations: `TrezorEmulator` and `GenericEmulator` (all other wallets).
- **`Bridge` trait** (`crates/bridge/src/lib.rs`) — UHID relay producing `InterceptedMessage` streams. `GenericBridge` supports TCP, Unix STREAM, and Unix DGRAM transports.
- **`GenericBridge`** (`crates/bridge/src/generic.rs`) — Relays HID reports between emulator sockets and `/dev/uhid` virtual devices. Supports connect-on-first-write and auto-reconnect. Strips UHID report ID byte (0x00) before forwarding to emulators.
- **`BundleManager`** (`crates/bundler/src/lib.rs`) — Facade over `BundleStorage` + `GithubDownloader`. Bundles live at `~/.hwwtui/bundles/{wallet}/manifest.json`.

### Event loop (crates/hwwtui/src/main.rs)

Each 100ms tick: draw frame → poll bridge → drain firmware logs → update download progress → poll debug-link screen (Trezor, ~500ms throttle) → handle input/mouse events → process queued actions.

### Rendering

`crates/hwwtui/src/ui.rs` — Two-panel split layout with tabbed views:
- **Left panel**: Controls, Screen/Output, Key Bindings (keys 1-3)
- **Right panel**: Methods, Firmware, Raw, Bridge (keys 5-8)
- **Click zones**: Mouse support for device tabs and panel tabs
- **Status bar**: Device state, transport, bridge status, key hints

### UHID Bridge Architecture

```
Desktop App (hidapi) ↔ /dev/hidraw ↔ /dev/uhid ↔ GenericBridge ↔ Emulator
```

- **TCP transport** (BitBox02): Eager connect, two tasks (read/write), report ID stripping
- **Unix DGRAM** (Coldcard): Connect-on-first-write, bound client socket (MicroPython requires explicit path for sendto), auto-reconnect
- **UHID thread**: Blocking thread polls `/dev/uhid` at 1ms intervals for output reports. Shutdown via `oneshot::Receiver` — handles both `Ok(())` and `Err(Closed)`.

### Wallet-specific notes

- **Trezor**: Direct UDP, debug link on port+1. `TrezorWireClient` supports Initialize, GetFeatures, LoadDevice, GetPublicKey.
- **BitBox02**: `bitbox-api` crate for initialization (noise protocol). Uses `spawn_blocking` + `std::thread::spawn` with dedicated `new_multi_thread` runtime (bitbox-api does blocking I/O in async fns).
- **Coldcard**: Headless via bash launcher (opens /dev/null fds for display/LED pipes). `MICROPYPATH` needs leading colon to append (not replace) frozen module path. Version patched before compilation.
- **Specter**: `SDL_VIDEODRIVER=dummy` to prevent segfault. USB VCP disabled by default — patched to enabled. Monkey-patches `Specter.setup` to inject test mnemonic. Gentle TCP probe (RST kills VCP server).
- **Ledger**: Speculos Docker, model `nanosp`. No UHID bridge — direct TCP discovery.
- **Jade**: QEMU ESP32 Docker. No UHID bridge — direct TCP discovery via `JadeEmulator::try_connect()`.

### Configuration

Environment variables defined in `crates/hwwtui/src/config.rs`:
- `HWWTUI_GITHUB_REPO` (default: `n1rna/hwwtui`) — repo for bundle downloads
- `TREZOR_FIRMWARE_PATH` — local trezor-firmware/core path (bypasses bundle)
- `TREZOR_PROFILE_DIR` (default: `/tmp/hwwtui-trezor`) — emulator state directory
- `TREZOR_PORT` (default: `21324`) — UDP port

### Bundle build system

`scripts/build/{wallet}.sh` — per-wallet build scripts. `scripts/docker/Dockerfile.{wallet}` — isolated build environments. `just bundle-test {wallet}` builds in Docker; `just bundle-install {wallet}` installs locally.

Key build notes:
- **Coldcard**: `rsync -L` to follow symlinks, exclude broken symlinks (`l-port`, `l-mpy`), patch version before compilation
- **Ledger/Jade**: Use Docker at runtime (Speculos / QEMU), not just build time
- **Specter**: Patch `hosts/usb.py` to enable USB by default

### Permissions

`udev/99-hwwtui.rules` — udev rules for `/dev/uhid` and wallet hidraw devices. Supports both real USB devices (`ATTRS{idVendor}`) and UHID virtual devices (`KERNELS` matching `0003:VID:PID.*`). Uses `TAG+="uaccess"` for systemd/logind auto-grant.

`just setup-udev` installs rules, sets `/dev/uhid` permissions, adds user to `plugdev` group.

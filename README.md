# hwwtui

[![CI](https://github.com/n1rna/hwwtui/actions/workflows/ci.yaml/badge.svg)](https://github.com/n1rna/hwwtui/actions/workflows/ci.yaml)
[![Rust](https://1tt.dev/badge/rust-1.80+-orange.svg?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Wallets](https://1tt.dev/badge/wallets-6-blue.svg?style=flat)](https://github.com/n1rna/hwwtui#supported-wallets)
[![License](https://1tt.dev/badge/license-MIT-green.svg)](https://github.com/n1rna/hwwtui/blob/main/LICENSE)
[![Linux](https://1tt.dev/badge/platform-linux-lightgrey.svg?logo=linux&logoColor=white)](https://github.com/n1rna/hwwtui)

A terminal UI lab for running, controlling, and inspecting hardware wallet emulators. Manages emulator processes for six wallet types (Trezor, BitBox02, Coldcard, Specter DIY, Ledger, Jade), captures their output, and bridges them to desktop wallet applications via UHID virtual HID devices.

## Quick Start

```bash
# Install udev rules (one-time, for UHID bridge)
just setup-udev

# Build
just build

# Run (downloads bundles on first use via [d] key)
just run

# Tail logs in another terminal
just logs
```

### Per-wallet workflows

**Trezor** (UDP, no bridge needed):
1. Press `d` to download bundle, `s` to start
2. Press `l` to load test seed
3. Desktop apps connect via UDP :21324

**BitBox02** (TCP + UHID bridge):
1. Press `d` to download, `s` to start (UHID bridge auto-starts)
2. Press `l` to initialize with test mnemonic
3. Desktop apps discover via hidapi (VID=0x03EB PID=0x2403)

**Coldcard** (DGRAM Unix socket + UHID bridge):
1. Press `d` to download, `s` to start (UHID bridge auto-starts)
2. Simulator starts pre-seeded — no initialization needed
3. Desktop apps discover via hidapi (VID=0xD13E PID=0xCC10)

**Ledger** (Docker/Speculos, direct TCP):
1. Press `d` to download, `s` to start (runs Speculos in Docker)
2. Starts pre-seeded with test mnemonic — no initialization needed
3. Desktop apps connect directly via TCP :9999

**Specter DIY** (MicroPython, direct TCP):
1. Press `d` to download, `s` to start
2. Auto-initializes with test mnemonic on startup
3. Desktop apps connect directly via TCP :8789

**Jade** (Docker/QEMU, direct TCP):
1. Press `d` to download, `s` to start (runs QEMU ESP32 in Docker)
2. Starts uninitialized — needs PIN setup via desktop app
3. Desktop apps connect directly via TCP :30121

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `HWWTUI_GITHUB_REPO` | `n1rna/hwwtui` | GitHub repo for bundle downloads |
| `TREZOR_FIRMWARE_PATH` | (none) | Path to local `trezor-firmware/core/` checkout (bypasses bundle) |
| `TREZOR_PROFILE_DIR` | `/tmp/hwwtui-trezor` | Emulator flash/SD state directory |
| `TREZOR_PORT` | `21324` | UDP port for the Trezor emulator |

## Keybindings

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Cycle device tabs |
| `1` `2` `3` | Switch left panel tab (Controls / Screen / Keys) |
| `5` `6` `7` `8` | Switch right panel tab (Methods / Firmware / Raw / Bridge) |
| `s` | Start selected emulator |
| `x` | Stop selected emulator |
| `r` | Reset (stop + start) |
| `d` | Download firmware bundle for selected device |
| `D` (Shift+D) | Remove installed bundle |
| `i` | Send Initialize command (Trezor) |
| `l` | Load test seed / initialize (Trezor, BitBox02) |
| `Enter` | Confirm / press YES via debug link (Trezor) |
| `Esc` | Cancel / press NO via debug link (Trezor) |
| `↑↓←→` | Swipe gesture via debug link (Trezor) |
| `q` / `Ctrl-C` | Quit (stops all emulators) |

Mouse: click on device tabs or panel tabs to switch.

## Supported Wallets

| Wallet | Transport | Bridge | Discovery | Desktop App |
|--------|-----------|--------|-----------|-------------|
| **Trezor** | UDP :21324 | None (direct UDP) | trezor-client | Working |
| **BitBox02** | TCP :15423 | UHID (VID 03EB) | hidapi | Working |
| **Coldcard** | Unix DGRAM | UHID (VID D13E) | hidapi | Working |
| **Specter DIY** | TCP :8789 | None (direct TCP) | TCP connect | Working |
| **Ledger** | TCP :9999 (Docker) | None (direct TCP) | TCP connect | Working |
| **Jade** | TCP :30121 (Docker) | None (direct TCP) | TCP connect | Working |

## UHID Bridge

For wallets that use HID communication (BitBox02, Coldcard), hwwtui creates virtual USB HID devices via Linux UHID (`/dev/uhid`). Desktop wallet applications discover these devices through `hidapi` exactly as they would discover real hardware.

```
Desktop App (hidapi) ↔ /dev/hidraw (kernel) ↔ /dev/uhid (UHID) ↔ GenericBridge ↔ Emulator
```

### Permissions

Run `just setup-udev` to install udev rules for `/dev/uhid` and hidraw devices. This is a one-time setup that:
- Grants access to `/dev/uhid` for virtual device creation
- Sets permissions on hidraw devices for BitBox02, Coldcard, Ledger, and Trezor VID/PIDs
- Works with both real hardware and UHID virtual devices

## Building Bundles

Each wallet's emulator is packaged as a downloadable bundle. To build locally:

```bash
# Build in Docker (recommended)
just bundle-test trezor
just bundle-test bitbox02
just bundle-test coldcard

# Install locally
just bundle-install trezor

# Ledger/Jade build on host (they use Docker internally)
./scripts/build/ledger-local.sh
./scripts/build/jade-local.sh
```

## Documentation

- [Architecture](docs/ARCHITECTURE.md) — workspace structure, data flow, key types
- [Wallet Reference](docs/WALLETS.md) — per-wallet details, transport, build deps, known issues
- [Development Guide](docs/DEVELOPMENT.md) — building, testing, CI, adding new wallets

## License

See repository root for license details.

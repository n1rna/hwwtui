# hwwtui

A terminal UI lab for running, controlling, and inspecting hardware wallet emulators. Manages emulator processes for six wallet types (Trezor, BitBox02, Coldcard, Specter DIY, Ledger, Jade), captures their output, and provides debug-link interaction -- all from a single terminal.

```
 ┌─ hwwtui ──────────────────────────────────────────────────────────┐
 │ [Trezor ▶]  [BitBox02 ■]  [Coldcard ○]  [Specter ○]  ...       │
 ├──────────────────────────────┬──────────────────────────────────── │
 │  Trezor — Screen             │  Method Calls                      │
 │                               │   → Emulator started (UDP :21324) │
 │    Confirm Transaction        │   → Debug link connected           │
 │    Send 0.001 BTC             │   → Initialize → Features (42 B)  │
 │    to bc1q...                 │   → LoadDevice → Success           │
 │                               │                                    │
 │   [Cancel]  [Confirm]         │                                    │
 ├───────────────────────────────┤────────────────────────────────────┤
 │  Controls                     │  Firmware Log                      │
 │  [s] Start  [x] Stop  [r]    │  trezor.loop DEBUG spawn...        │
 │  [d] Download  [D] Remove     │  trezor.workflow DEBUG start...    │
 │  [Enter] Confirm  [Esc]       │                                    │
 │                               │                                    │
 │  Bundle:     v2.8.9 (40 MB)  │                                    │
 │  Status:     Running          │                                    │
 │  Transport:  UDP :21324       │                                    │
 │  UHID:       —                │                                    │
 ├───────────────────────────────┴────────────────────────────────────┤
 │  Raw Messages                                                      │
 │  >> 3f 23 23 00 00 ...  << 3f 23 23 00 11 ...                     │
 └────────────────────────────────────────────────────────────────────┘
```

## Quick Start

```bash
# Build
just build

# Run (downloads bundles on first use via [d] key)
just run

# Or run directly
cargo run -p hwwtui

# Tail logs in another terminal
just logs
```

### First-time Trezor workflow

1. Launch `just run`
2. Press `d` to download the Trezor firmware bundle from GitHub Releases
3. Press `s` to start the emulator
4. Press `l` to load a test seed (12-word "abandon" mnemonic)
5. Press `Enter` to confirm any on-screen prompts
6. The emulator is now ready -- external apps (e.g. trezor-client, sigvault-desktop) can connect via UDP :21324

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
| `s` | Start selected emulator |
| `x` | Stop selected emulator |
| `r` | Reset (stop + start) |
| `d` | Download firmware bundle for selected device |
| `D` (Shift+D) | Remove installed bundle |
| `i` | Send Initialize command (Trezor only) |
| `l` | Load test seed onto emulator (Trezor only) |
| `Enter` | Confirm / press YES via debug link |
| `Esc` | Cancel / press NO via debug link |
| `Up/Down/Left/Right` | Swipe gesture via debug link |
| `q` / `Ctrl-C` | Quit (stops all emulators) |

## Supported Wallets

| Wallet | Transport | Emulator Type | Screen | Desktop App Integration |
|--------|-----------|---------------|--------|------------------------|
| **Trezor** | UDP :21324 | Native unix (MicroPython + C + Rust) | Debug link (text) | Works (UDP auto-discovery) |
| **BitBox02** | TCP :15423 | Native simulator (C + Rust) | Not available | Needs UHID bridge |
| **Coldcard** | Unix socket | MicroPython unix port | Not available | Needs UHID bridge |
| **Specter DIY** | TCP :8789 | MicroPython unix port + SDL2 | Not available | Needs UHID bridge |
| **Ledger** | TCP :9999 | Speculos (QEMU ARM) via Docker | Not available | Needs UHID bridge |
| **Jade** | TCP :30121 | QEMU Xtensa via Docker | Not available | Needs UHID bridge |

## Documentation

- [Architecture](docs/ARCHITECTURE.md) -- workspace structure, data flow, key types
- [Wallet Reference](docs/WALLETS.md) -- per-wallet details, transport, build deps, known issues
- [Development Guide](docs/DEVELOPMENT.md) -- building, testing, CI, adding new wallets
- [TODO / Known Issues](docs/TODO.md) -- planned work and current limitations

## License

See repository root for license details.

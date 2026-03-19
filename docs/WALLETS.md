# Wallet Reference

Per-wallet technical details for each supported hardware wallet emulator.

---

## Trezor

**Status**: Most complete. Full emulator lifecycle, debug link screen reading, wire commands, bundle download.

### Emulator

- **Type**: Native unix port of Trezor Core firmware (MicroPython + C + Rust)
- **Binary**: `trezor-emu-core` (built from `trezor-firmware/core/` via `make build_unix`)
- **Firmware version**: Pinned to `core/v2.8.9` in CI
- **Rust toolchain**: Requires `nightly-2025-06-01` (later nightlies break `panic_immediate_abort`)

### Transport

- **Main port**: UDP :21324 (configurable via `TREZOR_PORT` env var)
- **Debug link port**: UDP :21325 (always main port + 1)
- **Protocol**: 64-byte HID reports framed over UDP. First packet: `0x3F 0x23 0x23` + msg_type(u16 BE) + payload_len(u32 BE) + up to 55 bytes. Continuation: `0x3F` + up to 63 bytes.

### Debug Link

The debug link (`crates/protocol/src/trezor_debug.rs`) provides:

- **Screen reading**: `DebugLinkGetState` (msg type 101) returns `DebugLinkState` (msg type 102) with field 13 (`tokens`) containing JSON layout. Polled every ~500ms.
- **Button presses**: `DebugLinkDecision` (msg type 100) field 1 = button (0=No, 1=Yes, 2=Info)
- **Swipe gestures**: `DebugLinkDecision` field 2 = swipe direction (0=Up, 1=Down, 2=Left, 3=Right)
- **Text input**: `DebugLinkDecision` field 3 = string

### Wire Commands

`TrezorWireClient` (`crates/protocol/src/trezor_debug.rs`) sends commands to the main port:
- `Initialize` (type 0) → returns `Features` response
- `LoadDevice` (type 13) → loads mnemonic + label, auto-confirms `ButtonRequest` via debug link
- `Ping` (type 1), `GetFeatures` (type 55)

### Bundle Contents

```
~/.hwwtui/bundles/trezor/
├── manifest.json
├── trezor-emu-core        # Main binary
├── lib/                   # Bundled .so files (SDL2, etc.)
├── src/                   # Python modules (MICROPYPATH)
├── run.sh                 # LD_LIBRARY_PATH wrapper
└── bundle-info.json       # Build metadata
```

### Environment Variables (set on child process)

| Variable | Value | Purpose |
|---|---|---|
| `TREZOR_PROFILE_DIR` | `/tmp/hwwtui-trezor` | Flash/SD card state |
| `SDL_VIDEODRIVER` | `dummy` | Suppress display window |
| `TREZOR_UDP_PORT` | `21324` | Main transport port |
| `LD_LIBRARY_PATH` | `<bundle>/lib/` | Bundled shared libs |
| `MICROPYPATH` | `<bundle>/src/` | Python module path |

### Desktop App Integration

trezor-client (used by sigvault-desktop) discovers the emulator automatically via its UDP transport -- no UHID bridge needed. Just start the emulator and press `l` to load a test seed.

### Known Issues

- **UHID bridge disabled**: `trezor-client` uses `rusb` (WebUSB) to enumerate USB and detects the UHID virtual device as a real Trezor, then fails. The bridge code exists (`crates/bridge/src/trezor.rs`) but is not wired up in `app.rs`.
- **System deps**: Needs `python-is-python3` and `libsdl2-image-dev` for the build
- **Nightly pinning**: Must use `nightly-2025-06-01` for `core/v2.8.9` compatibility
- **First-start log timing**: Sometimes the firmware log panel is empty on first start due to a race between process spawn and stdout capture task

---

## BitBox02

**Status**: Emulator starts and runs. No debug link or screen display. Desktop app integration requires UHID bridge (not yet implemented).

### Emulator

- **Type**: Native simulator (C + Rust), built from `bitbox02-firmware/`
- **Binary**: `bitbox02-simulator` (built via `make simulator`)
- **Firmware repo**: `BitBoxSwiss/bitbox02-firmware` (submodules required)

### Transport

- **Port**: TCP :15423 (passed via `--port 15423` argument)
- **Protocol**: Protobuf over Noise framework (XX handshake)

### Screen

Not available. BitBox02 has no debug link equivalent. The screen panel shows "Waiting for screen layout..." when running.

### Bundle Contents

```
~/.hwwtui/bundles/bitbox02/
├── manifest.json
├── bitbox02-simulator     # Main binary
└── bundle-info.json
```

### Build Dependencies

```
cmake ninja-build gcc g++ protobuf-compiler
libusb-1.0-0-dev libudev-dev libcmocka-dev libhidapi-dev libprotobuf-dev libclang-dev
cbindgen bindgen-cli prost-build-proto (cargo install)
rust-src component (for BitBox02's pinned Rust toolchain)
```

### Desktop App Integration

BitBox02 desktop apps use `hidapi` for USB discovery. They cannot discover a TCP socket. Requires a TCP-to-UHID bridge (not yet implemented) to expose the simulator as a virtual USB HID device.

### Known Issues

- **Health check probe**: The TCP health check (used during startup polling) connects and immediately disconnects. The simulator interprets this as a client connection/disconnection cycle. Can cause the simulator to exit or enter an error state.
- **Noise pairing**: First connection from an app requires Noise XX handshake pairing. The simulator has no UI for confirmation, so it may auto-accept.

---

## Coldcard

**Status**: Bundle build script exists. Emulator launch wiring is in app.rs but not fully tested.

### Emulator

- **Type**: MicroPython unix port (custom build from `Coldcard/firmware`)
- **Binary**: `micropython` (built from `micropython/ports/unix/`)
- **Launch**: `micropython -i ./unix/simulator.py` with `MICROPYPATH` set to `shared/`

### Transport

- **Type**: Unix domain socket
- **Path**: `/tmp/ckcc-simulator.sock`

### Screen

Not available. No debug link equivalent.

### Bundle Contents

```
~/.hwwtui/bundles/coldcard/
├── manifest.json
├── micropython            # Main binary
├── shared/                # Python modules (MICROPYPATH)
└── unix/                  # simulator.py and related files
```

### Build Dependencies

```
python3 python3-dev python3-pip python3-venv python-is-python3
libffi-dev build-essential autoconf automake libtool libltdl-dev rsync
```

### Known Issues

- **GCC 13 compatibility**: The micropython Makefiles use `-Werror`, which breaks with GCC 13 due to new warnings. Need to patch the Makefiles to remove `-Werror`.
- **mpy-cross**: Must be built first before building the unix port
- **libltdl-dev**: Required but not always obvious
- **simulator.py launch**: The exact command and MICROPYPATH configuration may need adjustment depending on the firmware version
- **Not fully tested**: The emulator launch path in `app.rs` sets up the environment but end-to-end operation has not been verified

---

## Specter DIY

**Status**: Bundle build script exists. Emulator launch wiring is in app.rs but not fully tested.

### Emulator

- **Type**: MicroPython unix port with SDL2 GUI (from `cryptoadvance/specter-diy`)
- **Binary**: `micropython_unix`
- **Launch**: Binary with `MICROPYPATH` set to `src/`

### Transport

- **USB simulation**: TCP :8789
- **QR scanner simulation**: TCP :5941

### Bundle Contents

```
~/.hwwtui/bundles/specter/
├── manifest.json
├── micropython_unix       # Main binary
├── boot.py                # Boot script
└── src/                   # Python source (MICROPYPATH)
```

### Build Dependencies

```
python3 python3-pip libsdl2-dev
gcc-arm-none-eabi build-essential
pkg-config libtool autoconf automake
```

### Desktop App Integration

Not tested. Would likely need a TCP-to-UHID bridge for USB-based desktop apps.

### Known Issues

- **Not fully tested** with desktop apps
- **SDL2 required**: The unix build links against SDL2 for display

---

## Ledger

**Status**: Docker-based build. Bundle produces an ELF + Docker Compose setup. Not tested end-to-end.

### Emulator

- **Type**: Speculos (QEMU ARM emulator) running the Bitcoin app ELF
- **Firmware repo**: `LedgerHQ/app-bitcoin-new`
- **Runtime**: Requires Docker (Speculos runs inside a container)

### Transport

- **APDU port**: TCP :9999
- **HTTP API**: TCP :5000 (Speculos REST API)

### Bundle Contents

```
~/.hwwtui/bundles/ledger/
├── manifest.json
├── bitcoin-app.elf         # Compiled Bitcoin app
├── speculos-runner.sh      # Script that starts Docker container
└── docker-compose.yml      # Container configuration
```

### Launch

The `GenericEmulator` runs `speculos-runner.sh` from the bundle directory. This script starts the Speculos Docker container with the Bitcoin app ELF. Startup timeout is set to 30 seconds to account for Docker/QEMU boot time.

### Desktop App Integration

Would need a TCP-to-UHID bridge. Ledger desktop apps use `hidapi` for USB discovery.

### Known Issues

- **Docker dependency**: Requires Docker at runtime, not just for building
- **Startup time**: Slower than native emulators due to QEMU + Docker overhead
- **Not tested end-to-end**: The full pipeline (build → bundle → install → start → interact) has not been validated
- **CI build timeout**: 60 minutes allowed

---

## Jade

**Status**: Docker-based build. Heaviest bundle (~1.9 GB). Not tested end-to-end.

### Emulator

- **Type**: QEMU Xtensa running Jade firmware in Docker
- **Firmware repo**: `Blockstream/Jade`
- **Runtime**: Requires Docker

### Transport

- **Serial-over-TCP**: TCP :30121

### Bundle Contents

```
~/.hwwtui/bundles/jade/
├── manifest.json
├── jade-qemu              # Docker image export (large)
└── jade-runner.sh         # Script that loads Docker image and starts container
```

### Launch

The `GenericEmulator` runs `jade-runner.sh` which loads the QEMU Docker image and starts the container. Startup timeout is 30 seconds.

### Desktop App Integration

Jade desktop apps use serial port discovery. The TCP port would need bridging to a virtual serial device or UHID.

### Known Issues

- **Bundle size**: ~1.9 GB due to QEMU Xtensa Docker image
- **Docker dependency**: Requires Docker at runtime
- **Not tested end-to-end**
- **CI build timeout**: 90 minutes allowed (heaviest build)

# TODO / Known Issues

## UHID Bridge

### Trezor UHID bridge disabled
- **What**: `TrezorBridge` code exists in `crates/bridge/src/trezor.rs` but is not wired up in `app.rs` (the `bridge_result` is hard-coded to `None`)
- **Why**: `trezor-client` uses `rusb` (WebUSB) to enumerate USB devices. It detects the UHID virtual device as a real Trezor, connects to it, and fails because the UHID→UDP relay timing doesn't match real USB behavior.
- **Workaround**: Trezor emulator is accessed directly via UDP by `trezor-client`, which works without any bridge.
- **Fix needed**: Either patch `trezor-client` to skip UHID devices, or add a VID/PID that `trezor-client` doesn't recognize but `hidapi`-based tools do.

### TCP/Unix-to-UHID bridge not implemented
- **Affects**: BitBox02, Specter DIY, Ledger (all use `hidapi` for device discovery)
- **What's needed**: A bridge that connects to the emulator's TCP/Unix socket and exposes it as a UHID virtual HID device, similar to `TrezorBridge` but for TCP instead of UDP.
- **Blocking**: These wallets can run their emulators but desktop apps cannot discover them.

## BitBox02

### Simulator exits on health check disconnect
- **What**: During startup, `GenericEmulator::probe_once()` opens a TCP connection to port 15423 and immediately closes it. The BitBox02 simulator interprets this as a client connecting and disconnecting, which can cause it to exit or enter an error state.
- **Fix needed**: Either change the health check to something less disruptive (e.g., check if the port is listening without completing a TCP handshake) or configure the simulator to tolerate disconnect after connect.

## Coldcard

### Simulator launch not fully tested
- **What**: The `start_selected()` path for Coldcard constructs a `GenericEmulator` with the micropython binary, `MICROPYPATH`, and `simulator.py` arguments, but this has not been validated end-to-end.
- **Concerns**: `MICROPYPATH` may need adjustment, and `simulator.py` may expect to be launched from a specific working directory. GCC 13 `-Werror` issue in micropython Makefiles needs patching during the build.

## Specter DIY

### Not tested with desktop apps
- **What**: The emulator starts but integration with Specter desktop software has not been validated.
- **Transport**: Uses TCP :8789 for USB simulation and TCP :5941 for QR scanner. Desktop apps may expect USB HID, which would need a UHID bridge.

## Ledger / Jade

### Docker-based, not tested end-to-end
- **What**: Both use Docker containers at runtime (Speculos for Ledger, QEMU Xtensa for Jade). The full pipeline (build bundle → install → start → interact → desktop app) has not been validated.
- **Jade bundle size**: ~1.9 GB due to the QEMU Docker image export.
- **Startup time**: Slower than native emulators. 30-second startup timeout is configured but may not be sufficient.

## Screen Display

### Only Trezor has screen content
- **What**: The debug link screen reading (`DebugLinkGetState` → JSON tokens) is Trezor-specific. All other wallets show a blank/placeholder screen panel when running.
- **Fix needed**: Investigate whether other emulators expose screen state via any mechanism. Most likely they do not, and the screen panel should either show a clear "not available" message or be hidden for those wallets.

### Text-only rendering
- **What**: The screen panel shows extracted text (title, paragraphs, buttons) from the Trezor debug link JSON. No pixel-level rendering of the actual display.
- **Enhancement**: Could render a pixel representation using Unicode block characters, but would require decoding the actual framebuffer data (not currently exposed by the debug link in a convenient format).

## Firmware Log

### First-start output sometimes missing
- **What**: When starting an emulator for the first time, the firmware log panel may be empty for a few seconds. This is a timing issue: the stdout/stderr capture tasks start after the process spawns, and some initial output may be written before the async reader is ready.
- **Impact**: Minor UX issue. Logs appear after a short delay.

## Bundle System

### No auto-update
- **What**: Once a bundle is downloaded, there is no mechanism to check for newer versions or auto-update. The user must manually press `D` (remove) then `d` (download) to get a newer version.

### No version pinning per wallet
- **What**: The bundler always fetches the "latest" GitHub release. There is no way to pin a specific version for a specific wallet.

### GitHub Release not yet created
- **What**: The `build-bundles.yaml` CI workflow is configured but no release has been published yet. Pressing `d` in the TUI will fail with a GitHub API error until a release exists.

### Single platform only
- **What**: Asset names include platform (`linux-x86_64`, `macos-aarch64`, etc.) and the downloader filters by `current_platform()`. However, CI currently only builds `linux-x86_64` bundles. macOS and ARM builds are not set up.

## Missing Features

### Multi-device simultaneous running
- **What**: While the UI shows all wallet tabs, only one emulator can be meaningfully interacted with at a time (debug link polling is only done for the selected tab). Running multiple emulators simultaneously is technically possible but untested.

### Persistent emulator profiles
- **What**: Emulator state (flash, SD card) is stored in `/tmp/hwwtui-{wallet}/` which is ephemeral. No mechanism to save/restore named profiles.

### Automated test scenarios
- **What**: No scripted test flows (e.g., "start emulator → load seed → get address → verify"). All interaction is manual via keybindings.

### Protocol decoding
- **What**: Only Trezor has header-level wire protocol decoding (message type + payload length). Full protobuf parsing is not implemented. Other wallets have no protocol decoding at all.

## Code Quality

### Dead code warnings suppressed
- **Several fields and methods** have `#[allow(dead_code)]` attributes, indicating partial implementations that are not yet wired up (e.g., `DevicePane::implemented`, `DeviceConfig::enabled`, `GenericEmulator::profile_dir`).

### No integration tests
- **What**: Only unit tests exist. No integration tests that start an actual emulator process (would require either a mock or a real build).

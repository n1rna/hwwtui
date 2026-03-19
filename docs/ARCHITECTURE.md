# Architecture

## Workspace Structure

hwwtui is a Rust workspace with five crates:

```
hwwtui/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── hwwtui/                   # Binary crate: TUI app, event loop, rendering
│   │   └── src/
│   │       ├── main.rs           # Entry point, tracing setup, crossterm terminal
│   │       ├── app.rs            # App state, action dispatch, emulator/bridge lifecycle
│   │       ├── ui.rs             # Ratatui rendering (tabs, screen, controls, logs)
│   │       └── config.rs         # Env-based configuration (Config, DeviceKind, TrezorConfig)
│   ├── emulators/                # Emulator process management
│   │   └── src/
│   │       ├── lib.rs            # Emulator trait, EmulatorStatus, WalletType, TransportConfig
│   │       ├── trezor.rs         # TrezorEmulator: spawn micropython, UDP health check
│   │       └── generic.rs        # GenericEmulator: covers all non-Trezor wallets (TCP/Unix)
│   ├── bridge/                   # UHID virtual HID device layer
│   │   └── src/
│   │       ├── lib.rs            # Bridge trait, HidReport, InterceptedMessage, Direction
│   │       ├── trezor.rs         # TrezorBridge: UDP ↔ UHID bidirectional relay
│   │       └── uhid.rs           # VirtualHidDevice: /dev/uhid wrapper (uhid-virt)
│   ├── protocol/                 # Wire protocol decoders
│   │   └── src/
│   │       ├── lib.rs            # DecodedMessage type
│   │       ├── trezor.rs         # Trezor HID framing decoder (header-only, no protobuf)
│   │       └── trezor_debug.rs   # Debug link client + wire client + screen layout parser
│   └── bundler/                  # Firmware bundle download & storage
│       └── src/
│           ├── lib.rs            # BundleManager facade, BundleStatus, RemoteBundle, helpers
│           ├── download.rs       # GithubDownloader, tarball extraction, asset name parsing
│           ├── storage.rs        # BundleStorage: ~/.hwwtui/bundles/ layout, manifest I/O
│           └── manifest.rs       # BundleManifest struct (JSON serializable)
├── scripts/
│   ├── build/                    # Per-wallet build scripts (trezor.sh, bitbox02.sh, etc.)
│   └── docker/                   # Dockerfiles for isolated builds (Dockerfile.trezor, etc.)
└── justfile                      # Task runner recipes
```

## Crate Dependency Graph

```
hwwtui (binary)
  ├── emulators    (Emulator trait, process spawning)
  ├── bridge       (UHID bridge, needs emulators + protocol)
  │   ├── emulators
  │   └── protocol
  ├── protocol     (wire decoders)
  └── bundler      (download/storage, needs emulators for WalletType)
      └── emulators
```

## Data Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                        hwwtui TUI                               │
│                                                                 │
│  Event Loop (100ms tick)                                        │
│  ├── terminal.draw(ui::render)    → ratatui frame               │
│  ├── app.poll_bridge_messages()   → drain InterceptedMessage    │
│  ├── app.poll_firmware_logs()     → drain emulator stdout/err   │
│  ├── app.poll_download_progress() → watch::Receiver<BundleStatus>│
│  ├── app.poll_screen()            → DebugLinkGetState (UDP)     │
│  ├── crossterm::event::poll()     → keyboard input              │
│  └── app.process_actions()        → start/stop/download/etc     │
└──────────┬──────────────┬─────────────┬─────────────────────────┘
           │              │             │
    ┌──────┴──────┐  ┌────┴────┐  ┌────┴─────┐
    │  Emulator   │  │ Bridge  │  │ Bundler  │
    │  Process    │  │ (UHID)  │  │          │
    │             │  │         │  │ GitHub   │
    │ stdout/err ─┤  │ HID ←→ │  │ Releases │
    │ (captured)  │  │ UDP/TCP │  │ → .tar.gz│
    └──────┬──────┘  └────┬────┘  │ → extract│
           │              │       │ → manifest│
    UDP/TCP/Unix      /dev/uhid   └──────────┘
    (transport)       (virtual)
```

### Emulator stdout/stderr capture

Each emulator (TrezorEmulator or GenericEmulator) pipes stdout and stderr into a shared `Arc<Mutex<VecDeque<String>>>` ring buffer (500 lines max). Tokio tasks read lines asynchronously via `BufReader::lines()`. The event loop calls `drain_output()` every tick to move lines into the per-pane `firmware_log` deque (200 entries max, ANSI-stripped).

### Debug link screen reading (Trezor only)

`TrezorDebugLink` sends `DebugLinkGetState` (msg type 101) over UDP port 21325 every ~500ms (throttled to every 5 ticks). The response `DebugLinkState` (msg type 102) contains field 13 (`tokens`), a repeated string that concatenates into a JSON layout description. `parse_layout_tokens()` extracts title, paragraphs, buttons into a `ParsedLayout` for rendering in the Screen panel.

### Wire protocol commands (Trezor only)

`TrezorWireClient` connects on-demand to the main UDP port (21324) for commands like `Initialize` and `LoadDevice`. Connections are ephemeral (created, used, dropped) so they don't interfere with external apps (e.g. sigvault-desktop) that also connect to the emulator.

`LoadDevice` triggers `ButtonRequest` confirmations -- the wire client handles these automatically by sending `ButtonAck` and pressing YES via the debug link.

## Key Types

### `App` (`crates/hwwtui/src/app.rs`)

Top-level application state. Owns a `Vec<DevicePane>` (one per wallet type), the selected tab index, a pending action queue, and a shared `BundleManager`. All mutation goes through `dispatch(Action)` → `process_actions()`.

### `DevicePane` (`crates/hwwtui/src/app.rs`)

Per-device state including:
- `emulator: Option<Box<dyn Emulator>>` -- the process manager
- `bridge: Option<Box<dyn Bridge>>` -- UHID bridge (currently unused)
- `debug_link: Option<TrezorDebugLink>` -- screen/button access
- `wire_port: Option<u16>` -- for on-demand wire commands
- `method_log`, `raw_log`, `firmware_log` -- capped VecDeques for TUI display
- `screen_title`, `screen_content`, `screen_buttons` -- parsed debug link output
- `bundle_status: BundleStatus` -- installation state
- `download_progress_rx: Option<watch::Receiver<BundleStatus>>` -- live download progress

### `Emulator` trait (`crates/emulators/src/lib.rs`)

```rust
#[async_trait]
pub trait Emulator: Send + Sync {
    fn wallet_type(&self) -> WalletType;
    fn status(&self) -> EmulatorStatus;
    fn transport(&self) -> TransportConfig;
    async fn start(&mut self) -> anyhow::Result<()>;
    async fn stop(&mut self) -> anyhow::Result<()>;
    async fn health_check(&self) -> bool;
    fn drain_output(&mut self) -> Vec<String>;
}
```

Two implementations:
- **`TrezorEmulator`**: Spawns `trezor-emu-core` binary, sets `TREZOR_PROFILE_DIR`, `SDL_VIDEODRIVER=dummy`, `TREZOR_UDP_PORT`. Health check via UDP probe (zero-byte send + recv timeout).
- **`GenericEmulator`**: Spawns any binary with configurable args/env. Health check via TCP connect or Unix socket connect. Used for BitBox02, Coldcard, Specter, Ledger, Jade.

### `Bridge` trait (`crates/bridge/src/lib.rs`)

```rust
#[async_trait]
pub trait Bridge: Send + Sync {
    async fn start(&mut self) -> anyhow::Result<mpsc::UnboundedReceiver<InterceptedMessage>>;
    async fn stop(&mut self) -> anyhow::Result<()>;
    fn is_running(&self) -> bool;
}
```

One implementation: `TrezorBridge` (currently disabled in `app.rs`). Relays UDP datagrams to/from a UHID virtual HID device on a dedicated blocking thread.

### `BundleManager` (`crates/bundler/src/lib.rs`)

Facade combining `BundleStorage` (local disk at `~/.hwwtui/bundles/`) with `GithubDownloader` (GitHub Releases API). Downloads stream progress via `tokio::sync::watch` channel. After extraction, writes a `manifest.json` with the binary path, version, size, and platform.

### `TransportConfig` (`crates/emulators/src/lib.rs`)

```rust
pub enum TransportConfig {
    Udp { host: String, port: u16 },   // Trezor
    Tcp { host: String, port: u16 },   // BitBox02, Specter, Ledger, Jade
    UnixSocket { path: PathBuf },       // Coldcard
}
```

## How Emulators Are Started

1. User presses `s` on a wallet tab
2. `app.start_selected()` checks if `pane.emulator` is already set
3. If not, looks up the installed bundle via `bundle_manager.emulator_binary_path(wallet_type)`
4. Constructs the appropriate emulator type:
   - Trezor → `TrezorEmulator::new_with_binary(bin_path, bundle_dir, profile_dir, port)`
   - Others → `GenericEmulator::new(wallet_type, bin_path, working_dir, profile_dir, transport).with_arg(...).with_env(...)`
5. Calls `emu.start()` which:
   - Spawns the child process with `tokio::process::Command`
   - Sets `LD_LIBRARY_PATH` if `lib/` dir exists (bundled shared libs)
   - Spawns async tasks to capture stdout/stderr
   - Polls for transport readiness (UDP probe or TCP connect) with timeout
   - Sets status to `Running` or `Error`
6. For Trezor, also connects `TrezorDebugLink` on port+1

## Bundle System

```
~/.hwwtui/bundles/
├── trezor/
│   ├── manifest.json         # BundleManifest (wallet_type, version, emulator_binary, etc.)
│   ├── trezor-emu-core       # Main binary
│   ├── lib/                  # Bundled shared libraries
│   ├── src/                  # Python modules loaded at runtime
│   └── run.sh                # Wrapper script (sets LD_LIBRARY_PATH)
├── bitbox02/
│   ├── manifest.json
│   └── bitbox02-simulator
└── ...
```

GitHub Releases assets follow the naming convention `hwwtui-{wallet}-{platform}.tar.gz`. The `GithubDownloader` fetches the latest release, parses asset names, and streams downloads with progress. Tarballs are extracted with top-level directory stripping.

## UHID Bridge Architecture

The UHID bridge creates a virtual USB HID device via `/dev/uhid` that appears as `/dev/hidraw*` to host applications. A dedicated blocking thread owns the `UHIDDevice` and handles both directions:

- **Emulator → Host**: UDP recv → channel → UHID input report write
- **Host → Emulator**: UHID output report poll → channel → UDP send

Currently **disabled for Trezor** because `trezor-client` uses `rusb` (WebUSB) to enumerate USB devices and would incorrectly detect the UHID virtual device as a real Trezor, then fail to communicate. The Trezor emulator is instead accessed directly via UDP.

Not yet implemented for other wallets (BitBox02, Specter, Ledger need it for desktop app integration since they use `hidapi`-based USB discovery).

Requires `/dev/uhid` access: `sudo setfacl -m u:$USER:rw /dev/uhid` or a udev rule.

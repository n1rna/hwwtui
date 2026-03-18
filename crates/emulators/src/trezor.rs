//! Trezor emulator process management.
//!
//! The Trezor emulator ships inside the `trezor-firmware` repository as a
//! MicroPython binary (`micropython`) compiled from `core/`. Point
//! `firmware_path` at the `core/` directory of a checked-out and built
//! `trezor-firmware` tree.
//!
//! The emulator listens on UDP port 21324 (default) for HID-like packets and
//! renders its screen over UDP 21325.
//!
//! # Environment variables set on the child process
//!
//! | Variable | Purpose |
//! |---|---|
//! | `TREZOR_PROFILE_DIR` | Where flash/SD-card images are stored |
//! | `SDL_VIDEODRIVER` | Set to `offscreen` to suppress a display window |

use std::collections::VecDeque;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use crate::{Emulator, EmulatorStatus, TransportConfig, WalletType};

// ── Constants ─────────────────────────────────────────────────────────────────

/// UDP port the Trezor emulator listens on for HID packets.
pub const DEFAULT_PORT: u16 = 21324;

/// How long to wait for the emulator to become ready after spawning.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Interval between health-check probes during startup.
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(200);

// ── Struct ────────────────────────────────────────────────────────────────────

/// Manages the lifecycle of a Trezor Core emulator process.
///
/// # Example
///
/// ```no_run
/// use emulators::trezor::TrezorEmulator;
/// use emulators::Emulator;
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let mut emu = TrezorEmulator::new(
///         "/path/to/trezor-firmware/core",
///         "/tmp/trezor-profile",
///         21324,
///     );
///     emu.start().await?;
///     println!("Trezor emulator running");
///     emu.stop().await?;
///     Ok(())
/// }
/// ```
pub struct TrezorEmulator {
    /// Path to the `core/` directory inside `trezor-firmware`.
    firmware_path: PathBuf,

    /// If set, use this exact binary path instead of deriving from firmware_path.
    binary_override: Option<PathBuf>,

    /// Directory used for emulated flash / SD-card state files.
    profile_dir: PathBuf,

    /// UDP port to listen on (default: 21324).
    port: u16,

    /// Handle to the running child process, if any.
    child: Option<Child>,

    /// Current lifecycle status.
    status: EmulatorStatus,

    /// Captured stdout/stderr lines from the emulator process.
    output_lines: Arc<Mutex<VecDeque<String>>>,
}

impl TrezorEmulator {
    /// Create a new manager. Does **not** spawn any process yet.
    ///
    /// `firmware_path` should point to the `core/` subdirectory of a built
    /// `trezor-firmware` checkout, e.g. `/home/user/trezor-firmware/core`.
    pub fn new(
        firmware_path: impl Into<PathBuf>,
        profile_dir: impl Into<PathBuf>,
        port: u16,
    ) -> Self {
        Self {
            firmware_path: firmware_path.into(),
            binary_override: None,
            profile_dir: profile_dir.into(),
            port,
            child: None,
            status: EmulatorStatus::Stopped,
            output_lines: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Create a new manager with an explicit binary path (for bundled emulators).
    pub fn new_with_binary(
        binary_path: impl Into<PathBuf>,
        firmware_path: impl Into<PathBuf>,
        profile_dir: impl Into<PathBuf>,
        port: u16,
    ) -> Self {
        Self {
            firmware_path: firmware_path.into(),
            binary_override: Some(binary_path.into()),
            profile_dir: profile_dir.into(),
            port,
            child: None,
            status: EmulatorStatus::Stopped,
            output_lines: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Returns the path to the emulator binary.
    fn micropython_path(&self) -> PathBuf {
        if let Some(ref bin) = self.binary_override {
            return bin.clone();
        }
        self.firmware_path
            .join("build")
            .join("unix")
            .join("trezor-emu-core")
    }

    /// Probe the UDP port once synchronously; used during the startup poll.
    fn probe_udp(&self) -> bool {
        // Bind an ephemeral local socket to send a zero-length ping.
        let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else {
            return false;
        };
        sock.set_read_timeout(Some(Duration::from_millis(100))).ok();
        let target = format!("127.0.0.1:{}", self.port);
        // A zero-byte send checks that the OS can reach the destination port.
        // The emulator ignores malformed packets, so this is safe.
        if sock.send_to(&[], &target).is_err() {
            return false;
        }
        // If the port is open, the emulator will either respond or simply not
        // send an ICMP port-unreachable (which the OS would surface as an
        // error on the *next* recv). Receiving a timeout means something
        // listened; an immediate connection error means nothing did.
        let mut buf = [0u8; 1];
        match sock.recv_from(&mut buf) {
            Ok(_) => true,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Timeout => port is open but emulator sent nothing back; that
                // is fine — it is running.
                true
            }
            Err(_) => false,
        }
    }

    /// Poll until the emulator port is reachable or the timeout expires.
    async fn wait_for_ready(&self) -> bool {
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        loop {
            if self.probe_udp() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
        }
    }
}

// ── Emulator impl ─────────────────────────────────────────────────────────────

#[async_trait]
impl Emulator for TrezorEmulator {
    fn wallet_type(&self) -> WalletType {
        WalletType::Trezor
    }

    fn status(&self) -> EmulatorStatus {
        self.status.clone()
    }

    fn transport(&self) -> TransportConfig {
        TransportConfig::Udp {
            host: "127.0.0.1".to_string(),
            port: self.port,
        }
    }

    async fn start(&mut self) -> anyhow::Result<()> {
        if matches!(
            self.status,
            EmulatorStatus::Running | EmulatorStatus::Starting
        ) {
            return Ok(());
        }

        let binary = self.micropython_path();
        if !binary.exists() {
            anyhow::bail!(
                "Trezor emulator binary not found at {}. \
                 Build trezor-firmware first: cd core && make build_unix",
                binary.display()
            );
        }

        std::fs::create_dir_all(&self.profile_dir)?;

        info!(
            binary = %binary.display(),
            profile_dir = %self.profile_dir.display(),
            port = self.port,
            "Starting Trezor emulator"
        );

        self.status = EmulatorStatus::Starting;

        // If the binary has a lib/ sibling directory (bundled builds), prepend
        // it to LD_LIBRARY_PATH so the emulator finds its shared libraries.
        let lib_dir = binary.parent().map(|p| p.join("lib"));
        let ld_library_path = match &lib_dir {
            Some(d) if d.is_dir() => {
                let existing = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
                if existing.is_empty() {
                    d.display().to_string()
                } else {
                    format!("{}:{existing}", d.display())
                }
            }
            _ => std::env::var("LD_LIBRARY_PATH").unwrap_or_default(),
        };

        // Set MICROPYPATH so the emulator finds its Python modules in src/.
        let src_dir = self.firmware_path.join("src");
        let micropypath = if src_dir.is_dir() {
            src_dir.display().to_string()
        } else {
            // Fallback: standard trezor-firmware layout
            String::new()
        };

        let mut child = Command::new(&binary)
            .current_dir(&self.firmware_path)
            .env("LD_LIBRARY_PATH", &ld_library_path)
            .env("MICROPYPATH", &micropypath)
            .env("TREZOR_PROFILE_DIR", &self.profile_dir)
            // Use the dummy SDL driver — never opens a window on any SDL2 version.
            .env("SDL_VIDEODRIVER", "dummy")
            // Bind to the configured port.
            .env("TREZOR_UDP_PORT", self.port.to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn Trezor emulator: {e}"))?;

        // Take stdout/stderr BEFORE moving child into self so we keep &mut Child.
        if let Some(stdout) = child.stdout.take() {
            let lines = Arc::clone(&self.output_lines);
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut stream = reader.lines();
                while let Ok(Some(line)) = stream.next_line().await {
                    let mut buf = lines.lock().unwrap();
                    if buf.len() >= 500 {
                        buf.pop_front();
                    }
                    buf.push_back(line);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let lines = Arc::clone(&self.output_lines);
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut stream = reader.lines();
                while let Ok(Some(line)) = stream.next_line().await {
                    let mut buf = lines.lock().unwrap();
                    if buf.len() >= 500 {
                        buf.pop_front();
                    }
                    buf.push_back(line);
                }
            });
        }

        self.child = Some(child);

        debug!("Waiting for Trezor emulator UDP port {}…", self.port);
        if self.wait_for_ready().await {
            info!("Trezor emulator is ready on UDP :{}", self.port);
            self.status = EmulatorStatus::Running;
        } else {
            warn!(
                "Trezor emulator did not become ready within {:?}",
                STARTUP_TIMEOUT
            );
            // Keep the child alive; the user can retry health_check later.
            self.status =
                EmulatorStatus::Error(format!("UDP :{} not reachable after startup", self.port));
        }

        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.child.take() {
            info!("Stopping Trezor emulator (pid {:?})", child.id());
            // Ask politely first.
            if let Some(pid) = child.id() {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
            // Give it 2 s to exit before we forcefully kill it.
            match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => debug!("Emulator exited with {status}"),
                _ => {
                    child.kill().await.ok();
                }
            }
        }
        self.status = EmulatorStatus::Stopped;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.probe_udp()
    }

    fn drain_output(&mut self) -> Vec<String> {
        let mut buf = self.output_lines.lock().unwrap();
        buf.drain(..).collect()
    }
}

// ── Drop ──────────────────────────────────────────────────────────────────────

impl Drop for TrezorEmulator {
    fn drop(&mut self) {
        // The Child has `kill_on_drop(true)` so the OS will clean up even if
        // stop() was never called, but we log a warning to make it visible.
        if self.child.is_some() {
            warn!("TrezorEmulator dropped while child process is still running");
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micropython_path_is_constructed_correctly() {
        let emu = TrezorEmulator::new("/opt/trezor-firmware/core", "/tmp/profile", 21324);
        let p = emu.micropython_path();
        assert_eq!(
            p,
            PathBuf::from("/opt/trezor-firmware/core/build/unix/trezor-emu-core")
        );
    }

    #[test]
    fn initial_status_is_stopped() {
        let emu = TrezorEmulator::new("/opt/trezor-firmware/core", "/tmp/profile", 21324);
        assert_eq!(emu.status(), EmulatorStatus::Stopped);
    }

    #[test]
    fn transport_uses_configured_port() {
        let emu = TrezorEmulator::new("/opt/trezor-firmware/core", "/tmp/profile", 9999);
        match emu.transport() {
            TransportConfig::Udp { host, port } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, 9999);
            }
            other => panic!("unexpected transport: {other:?}"),
        }
    }
}

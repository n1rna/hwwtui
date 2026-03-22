//! Generic emulator process manager.
//!
//! [`GenericEmulator`] covers every hardware wallet simulator whose transport
//! is either a TCP socket or a Unix-domain socket.  All five non-Trezor
//! devices (BitBox02, Coldcard, Specter, Ledger, Jade) are wired up through
//! this type.
//!
//! # Transport probing
//!
//! | [`TransportConfig`] variant | Readiness probe |
//! |---|---|
//! | `Tcp`        | `TcpStream::connect` with a short deadline |
//! | `UnixSocket` | path exists **and** `UnixStream::connect` succeeds |
//! | `Udp`        | not used here; handled by [`TrezorEmulator`] |
//!
//! # Shared-library discovery
//!
//! If `<working_dir>/lib/` exists the emulator prepends it to
//! `LD_LIBRARY_PATH` so that bundled `.so` files are found at runtime.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use crate::{Emulator, EmulatorStatus, TransportConfig, WalletType};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default time budget for the emulator to become reachable after spawning.
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

/// Interval between readiness probes during the startup poll loop.
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Per-connection timeout used by each TCP/Unix probe attempt.
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

/// Maximum number of output lines kept in the ring buffer.
const OUTPUT_RING_SIZE: usize = 500;

// ── Struct ────────────────────────────────────────────────────────────────────

/// Generic hardware-wallet emulator backed by a child process.
///
/// # Example — BitBox02
///
/// ```no_run
/// use std::path::PathBuf;
/// use std::time::Duration;
/// use emulators::generic::GenericEmulator;
/// use emulators::{Emulator, TransportConfig, WalletType};
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let mut emu = GenericEmulator::new(
///         WalletType::BitBox02,
///         PathBuf::from("/opt/hwwtui/bundles/bitbox02/bitbox02-simulator"),
///         PathBuf::from("/opt/hwwtui/bundles/bitbox02"),
///         PathBuf::from("/tmp/hwwtui-bitbox02"),
///         TransportConfig::Tcp { host: "127.0.0.1".into(), port: 15423 },
///     );
///     emu.start().await?;
///     println!("BitBox02 simulator running");
///     emu.stop().await?;
///     Ok(())
/// }
/// ```
pub struct GenericEmulator {
    /// Which hardware wallet model this instance represents.
    wallet_type: WalletType,

    /// Absolute path to the emulator binary (or shell script).
    binary_path: PathBuf,

    /// Current working directory for the child process.
    working_dir: PathBuf,

    /// Directory for emulated flash / state files.
    #[allow(dead_code)]
    profile_dir: PathBuf,

    /// How to reach the running emulator.
    transport: TransportConfig,

    /// Extra environment variables injected into the child process.
    env_vars: Vec<(String, String)>,

    /// Extra command-line arguments appended to the binary invocation.
    args: Vec<String>,

    /// How long to wait for the transport to become reachable.
    startup_timeout: Duration,

    /// Use a gentle TCP probe (normal close) instead of RST.
    /// Some emulators (e.g. Specter) close their TCP server after
    /// receiving too many RST packets.
    gentle_probe: bool,

    /// Handle to the running child process, if any.
    child: Option<Child>,

    /// Current lifecycle status.
    status: EmulatorStatus,

    /// Captured stdout/stderr lines from the child process.
    output_lines: Arc<Mutex<VecDeque<String>>>,
}

impl GenericEmulator {
    /// Create a new manager.  Does **not** spawn any process yet.
    pub fn new(
        wallet_type: WalletType,
        binary_path: impl Into<PathBuf>,
        working_dir: impl Into<PathBuf>,
        profile_dir: impl Into<PathBuf>,
        transport: TransportConfig,
    ) -> Self {
        Self {
            wallet_type,
            binary_path: binary_path.into(),
            working_dir: working_dir.into(),
            profile_dir: profile_dir.into(),
            transport,
            env_vars: Vec::new(),
            args: Vec::new(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            gentle_probe: false,
            child: None,
            status: EmulatorStatus::Stopped,
            output_lines: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Append an extra environment variable that will be set on the child
    /// process in addition to the automatic `LD_LIBRARY_PATH` handling.
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env_vars.push((key.to_string(), value.to_string()));
        self
    }

    /// Append an extra command-line argument to the binary invocation.
    pub fn with_arg(mut self, arg: &str) -> Self {
        self.args.push(arg.to_string());
        self
    }

    /// Override the startup timeout (default: [`DEFAULT_STARTUP_TIMEOUT`]).
    pub fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Use a gentle TCP probe (normal FIN close) instead of RST.
    /// Some emulators crash or close their server after repeated RST probes.
    pub fn with_gentle_probe(mut self) -> Self {
        self.gentle_probe = true;
        self
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Compute `LD_LIBRARY_PATH`, prepending `<working_dir>/lib/` when that
    /// directory exists (bundled shared-library layout).
    fn build_ld_library_path(&self) -> String {
        let lib_dir = self.working_dir.join("lib");
        let existing = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        if lib_dir.is_dir() {
            if existing.is_empty() {
                lib_dir.display().to_string()
            } else {
                format!("{}:{existing}", lib_dir.display())
            }
        } else {
            existing
        }
    }

    /// Probe the configured transport once.  Returns `true` when reachable.
    ///
    /// For TCP transports we use `SO_LINGER(0)` so the connection sends RST
    /// instead of FIN on close.  This prevents emulators like BitBox02 from
    /// interpreting the probe as a real client connect/disconnect cycle.
    async fn probe_once(&self) -> bool {
        match &self.transport {
            TransportConfig::Tcp { host, port } => {
                let addr_str = format!("{host}:{port}");
                let gentle = self.gentle_probe;
                tokio::task::spawn_blocking(move || {
                    use std::net::TcpStream;
                    let stream = TcpStream::connect_timeout(
                        &addr_str.parse().unwrap(),
                        PROBE_CONNECT_TIMEOUT,
                    );
                    match stream {
                        Ok(s) => {
                            if !gentle {
                                // Set SO_LINGER with timeout=0 so the socket sends
                                // RST instead of FIN on close.  This prevents
                                // emulators (e.g. BitBox02) from seeing a full
                                // client connect/disconnect cycle.
                                use std::os::unix::io::AsRawFd;
                                let linger = libc::linger {
                                    l_onoff: 1,
                                    l_linger: 0,
                                };
                                unsafe {
                                    libc::setsockopt(
                                        s.as_raw_fd(),
                                        libc::SOL_SOCKET,
                                        libc::SO_LINGER,
                                        &linger as *const libc::linger
                                            as *const libc::c_void,
                                        std::mem::size_of::<libc::linger>()
                                            as libc::socklen_t,
                                    );
                                }
                            }
                            // gentle mode: normal FIN close (just drop the socket)
                            true
                        }
                        Err(_) => false,
                    }
                })
                .await
                .unwrap_or(false)
            }
            TransportConfig::UnixSocket { path } => {
                if !path.exists() {
                    return false;
                }
                // Try STREAM first, then DGRAM (Coldcard uses DGRAM sockets).
                let stream_ok = tokio::time::timeout(
                    PROBE_CONNECT_TIMEOUT,
                    tokio::net::UnixStream::connect(path),
                )
                .await
                .map(|r| r.is_ok())
                .unwrap_or(false);

                if stream_ok {
                    return true;
                }

                // DGRAM probe: just check the socket file exists and is a socket.
                // We can't "connect" to verify the server is listening with DGRAM
                // the same way, but the file existing as a socket is sufficient.
                tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || {
                        use std::os::unix::net::UnixDatagram;
                        let client = match UnixDatagram::unbound() {
                            Ok(s) => s,
                            Err(_) => return false,
                        };
                        client.connect(&path).is_ok()
                    }
                })
                .await
                .unwrap_or(false)
            }
            TransportConfig::Udp { .. } => {
                // UDP probing is handled by TrezorEmulator; this type should
                // never be constructed with a UDP transport.
                false
            }
        }
    }

    /// Poll until the transport is reachable or `startup_timeout` expires.
    async fn wait_for_ready(&self) -> bool {
        let deadline = tokio::time::Instant::now() + self.startup_timeout;
        loop {
            if self.probe_once().await {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
        }
    }

    /// Push a line into the shared ring buffer, evicting the oldest entry when
    /// the buffer is full.
    fn push_line(lines: &Arc<Mutex<VecDeque<String>>>, line: String) {
        let mut buf = lines.lock().unwrap();
        if buf.len() >= OUTPUT_RING_SIZE {
            buf.pop_front();
        }
        buf.push_back(line);
    }
}

// ── Emulator impl ─────────────────────────────────────────────────────────────

#[async_trait]
impl Emulator for GenericEmulator {
    fn wallet_type(&self) -> WalletType {
        self.wallet_type
    }

    fn status(&self) -> EmulatorStatus {
        self.status.clone()
    }

    fn transport(&self) -> TransportConfig {
        self.transport.clone()
    }

    async fn start(&mut self) -> anyhow::Result<()> {
        if matches!(
            self.status,
            EmulatorStatus::Running | EmulatorStatus::Starting
        ) {
            return Ok(());
        }

        if !self.binary_path.exists() {
            anyhow::bail!(
                "{} emulator binary not found at {}",
                self.wallet_type,
                self.binary_path.display()
            );
        }

        std::fs::create_dir_all(&self.profile_dir)?;

        info!(
            wallet = %self.wallet_type,
            binary = %self.binary_path.display(),
            working_dir = %self.working_dir.display(),
            transport = %self.transport,
            "Starting emulator"
        );

        self.status = EmulatorStatus::Starting;

        let ld_library_path = self.build_ld_library_path();

        let mut cmd = Command::new(&self.binary_path);
        cmd.current_dir(&self.working_dir)
            .env("LD_LIBRARY_PATH", &ld_library_path);

        for (key, val) in &self.env_vars {
            cmd.env(key, val);
        }

        for arg in &self.args {
            cmd.arg(arg);
        }

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to spawn {} emulator ({}): {e}",
                    self.wallet_type,
                    self.binary_path.display()
                )
            })?;

        // Spawn reader tasks for stdout and stderr before moving child.
        if let Some(stdout) = child.stdout.take() {
            let lines = Arc::clone(&self.output_lines);
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut stream = reader.lines();
                while let Ok(Some(line)) = stream.next_line().await {
                    Self::push_line(&lines, line);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let lines = Arc::clone(&self.output_lines);
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut stream = reader.lines();
                while let Ok(Some(line)) = stream.next_line().await {
                    Self::push_line(&lines, line);
                }
            });
        }

        self.child = Some(child);

        debug!(
            wallet = %self.wallet_type,
            transport = %self.transport,
            timeout_secs = self.startup_timeout.as_secs(),
            "Waiting for emulator transport to become ready"
        );

        if self.wait_for_ready().await {
            info!(
                wallet = %self.wallet_type,
                transport = %self.transport,
                "Emulator is ready"
            );
            self.status = EmulatorStatus::Running;
        } else {
            warn!(
                wallet = %self.wallet_type,
                transport = %self.transport,
                timeout = ?self.startup_timeout,
                "Emulator transport not reachable within timeout"
            );
            self.status = EmulatorStatus::Error(format!(
                "{} not reachable after {}s",
                self.transport,
                self.startup_timeout.as_secs()
            ));
        }

        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.child.take() {
            info!(
                wallet = %self.wallet_type,
                pid = ?child.id(),
                "Stopping emulator"
            );
            if let Some(pid) = child.id() {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
            match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                Ok(Ok(status)) => debug!(wallet = %self.wallet_type, "Emulator exited: {status}"),
                _ => {
                    child.kill().await.ok();
                }
            }
        }
        self.status = EmulatorStatus::Stopped;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.probe_once().await
    }

    fn drain_output(&mut self) -> Vec<String> {
        let mut buf = self.output_lines.lock().unwrap();
        buf.drain(..).collect()
    }
}

// ── Drop ──────────────────────────────────────────────────────────────────────

impl Drop for GenericEmulator {
    fn drop(&mut self) {
        if self.child.is_some() {
            warn!(
                wallet = %self.wallet_type,
                "GenericEmulator dropped while child process is still running"
            );
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_emu(transport: TransportConfig) -> GenericEmulator {
        GenericEmulator::new(
            WalletType::BitBox02,
            PathBuf::from("/nonexistent/bitbox02-simulator"),
            PathBuf::from("/nonexistent"),
            PathBuf::from("/tmp/test-profile"),
            transport,
        )
    }

    #[test]
    fn initial_status_is_stopped() {
        let emu = make_emu(TransportConfig::Tcp {
            host: "127.0.0.1".into(),
            port: 15423,
        });
        assert_eq!(emu.status(), EmulatorStatus::Stopped);
    }

    #[test]
    fn wallet_type_is_preserved() {
        let emu = make_emu(TransportConfig::Tcp {
            host: "127.0.0.1".into(),
            port: 15423,
        });
        assert_eq!(emu.wallet_type(), WalletType::BitBox02);
    }

    #[test]
    fn transport_is_preserved() {
        let emu = make_emu(TransportConfig::Tcp {
            host: "127.0.0.1".into(),
            port: 15423,
        });
        match emu.transport() {
            TransportConfig::Tcp { host, port } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, 15423);
            }
            other => panic!("unexpected transport: {other:?}"),
        }
    }

    #[test]
    fn builder_methods_are_chainable() {
        let emu = make_emu(TransportConfig::UnixSocket {
            path: PathBuf::from("/tmp/test.sock"),
        })
        .with_env("MICROPYPATH", "/some/path")
        .with_arg("-i")
        .with_arg("simulator.py")
        .with_startup_timeout(Duration::from_secs(30));

        assert_eq!(emu.startup_timeout, Duration::from_secs(30));
        assert_eq!(emu.args, vec!["-i", "simulator.py"]);
        assert_eq!(
            emu.env_vars,
            vec![("MICROPYPATH".to_string(), "/some/path".to_string())]
        );
    }

    #[test]
    fn drain_output_returns_empty_when_no_output() {
        let mut emu = make_emu(TransportConfig::Tcp {
            host: "127.0.0.1".into(),
            port: 15423,
        });
        let out = emu.drain_output();
        assert!(out.is_empty());
    }

    /// Verify that `probe_once` sends RST (not FIN) by checking the listener
    /// does not observe a clean client session.
    #[tokio::test]
    async fn probe_once_rst_does_not_leave_session() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        // Bind a listener and count accepted connections that complete a
        // graceful shutdown (i.e. read returns Ok(0) indicating FIN).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let graceful_count = Arc::new(AtomicUsize::new(0));

        let gc = Arc::clone(&graceful_count);
        let handle = std::thread::spawn(move || {
            // Accept up to 3 connections with a short timeout.
            listener.set_nonblocking(false).unwrap();
            listener
                .set_nonblocking(false)
                .ok();
            for _ in 0..3 {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(200)))
                            .ok();
                        let mut buf = [0u8; 1];
                        use std::io::Read;
                        match (&stream).read(&mut buf) {
                            Ok(0) => {
                                // FIN received — graceful close.
                                gc.fetch_add(1, Ordering::SeqCst);
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                                // RST received — expected from our probe.
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut =>
                            {
                                // Timeout — connection dropped without data.
                            }
                            _ => {}
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let emu = make_emu(TransportConfig::Tcp {
            host: "127.0.0.1".into(),
            port,
        });

        // Probe three times.
        for _ in 0..3 {
            let result = emu.probe_once().await;
            assert!(result, "probe should succeed against a listening port");
        }

        // Give the listener thread time to process.
        tokio::time::sleep(Duration::from_millis(500)).await;
        drop(handle);

        // With RST probes, the graceful_count should be 0 (no FIN observed).
        let g = graceful_count.load(Ordering::SeqCst);
        assert_eq!(
            g, 0,
            "Expected 0 graceful closes (FIN), got {g}. Probe should send RST."
        );
    }
}

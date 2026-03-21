//! Generic TCP/Unix-to-UHID bridge.
//!
//! [`GenericBridge`] relays fixed-size HID reports between an emulator's
//! TCP (or Unix-domain) socket and a virtual HID device exposed through
//! `/dev/uhid`.  Desktop wallet applications (e.g. sigvault-desktop) discover
//! the virtual device via `hidapi` using the wallet-specific VID/PID.
//!
//! ```text
//!  Desktop App (hidapi) ↔ /dev/uhid (UHID) ↔ GenericBridge ↔ TCP ↔ Emulator
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info};

use crate::uhid::VirtualHidDevice;
use crate::{Bridge, Direction, InterceptedMessage};

// ── Config ──────────────────────────────────────────────────────────────────

/// Transport variant for the emulator side of the bridge.
#[derive(Debug, Clone)]
pub enum BridgeTransport {
    /// TCP socket at `host:port`.
    Tcp { host: String, port: u16 },
    /// Unix-domain socket at the given path.
    Unix { path: PathBuf },
}

/// Configuration for a [`GenericBridge`].
#[derive(Debug, Clone)]
pub struct GenericBridgeConfig {
    /// USB Vendor ID presented to the OS.
    pub vid: u16,
    /// USB Product ID presented to the OS.
    pub pid: u16,
    /// Human-readable device name shown in HID enumeration.
    pub name: String,
    /// Raw HID report descriptor bytes.
    pub report_descriptor: Vec<u8>,
    /// Size of each HID report in bytes (typically 64).
    pub report_size: usize,
    /// How to connect to the emulator.
    pub transport: BridgeTransport,
}

impl GenericBridgeConfig {
    pub fn new(
        vid: u16,
        pid: u16,
        name: impl Into<String>,
        report_descriptor: &[u8],
        transport: BridgeTransport,
    ) -> Self {
        Self {
            vid,
            pid,
            name: name.into(),
            report_descriptor: report_descriptor.to_vec(),
            report_size: 64,
            transport,
        }
    }

    /// Override the default report size (64 bytes).
    pub fn with_report_size(mut self, size: usize) -> Self {
        self.report_size = size;
        self
    }
}

// ── Bridge ──────────────────────────────────────────────────────────────────

/// Generic TCP/Unix-to-UHID bridge.
pub struct GenericBridge {
    config: GenericBridgeConfig,
    running: Arc<AtomicBool>,
    shutdown_tx: Vec<oneshot::Sender<()>>,
}

impl GenericBridge {
    pub fn new(config: GenericBridgeConfig) -> Self {
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            shutdown_tx: Vec::new(),
        }
    }

    /// Spawn the blocking thread that owns the UHID device.
    fn spawn_uhid_thread(
        mut uhid_dev: VirtualHidDevice,
        mut write_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        read_tx: mpsc::UnboundedSender<Vec<u8>>,
        mut shutdown_rx: oneshot::Receiver<()>,
    ) {
        std::thread::spawn(move || {
            debug!("GenericBridge UHID thread started");
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    debug!("GenericBridge UHID thread received shutdown");
                    break;
                }

                // Write: drain pending input reports (emulator → host).
                while let Ok(report) = write_rx.try_recv() {
                    if let Err(e) = uhid_dev.send_input_report(&report) {
                        error!("UHID input report write failed: {e}");
                        return;
                    }
                }

                // Read: poll for output reports (host → emulator).
                match uhid_dev.poll_output_report() {
                    Ok(Some(data)) => {
                        if read_tx.send(data).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!("UHID poll error: {e}");
                        break;
                    }
                }

                std::thread::sleep(Duration::from_millis(1));
            }
            debug!("GenericBridge UHID thread exiting");
        });
    }
}

#[async_trait::async_trait]
impl Bridge for GenericBridge {
    async fn start(&mut self) -> anyhow::Result<mpsc::UnboundedReceiver<InterceptedMessage>> {
        if self.running.load(Ordering::SeqCst) {
            anyhow::bail!("GenericBridge is already running");
        }

        let cfg = &self.config;
        info!(
            name = %cfg.name,
            vid = format!("{:#06x}", cfg.vid),
            pid = format!("{:#06x}", cfg.pid),
            "Starting GenericBridge"
        );

        let uhid_device = VirtualHidDevice::new(
            cfg.vid,
            cfg.pid,
            &cfg.name,
            &cfg.report_descriptor,
        )
        .with_context(|| format!("Failed to create UHID device '{}'", cfg.name))?;

        let (intercept_tx, intercept_rx) = mpsc::unbounded_channel::<InterceptedMessage>();
        let (shutdown_a_tx, mut shutdown_a_rx) = oneshot::channel::<()>();
        let (shutdown_b_tx, shutdown_b_rx) = oneshot::channel::<()>();

        self.shutdown_tx.push(shutdown_a_tx);
        self.shutdown_tx.push(shutdown_b_tx);
        self.running.store(true, Ordering::SeqCst);

        let (uhid_write_tx, uhid_write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (uhid_read_tx, mut uhid_read_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        Self::spawn_uhid_thread(uhid_device, uhid_write_rx, uhid_read_tx, shutdown_b_rx);

        let report_size = cfg.report_size;
        let transport = cfg.transport.clone();
        let running = Arc::clone(&self.running);

        // Connect eagerly and relay with two simple tasks.
        match transport {
            BridgeTransport::Tcp { host, port } => {
                let addr = format!("{host}:{port}");
                let stream = tokio::net::TcpStream::connect(&addr)
                    .await
                    .with_context(|| format!("Failed to connect to emulator at {addr}"))?;
                stream.set_nodelay(true).ok();
                let (mut read_half, mut write_half) = tokio::io::split(stream);

                // Task A: emulator → host (TCP read → UHID write)
                {
                    let tx = intercept_tx.clone();
                    let running = Arc::clone(&running);
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; report_size];
                        loop {
                            tokio::select! {
                                result = read_half.read_exact(&mut buf) => {
                                    match result {
                                        Ok(_) => {
                                            let data = buf.clone();
                                            let _ = tx.send(InterceptedMessage::new(
                                                Direction::DeviceToHost, &data, None,
                                            ));
                                            let _ = uhid_write_tx.send(data);
                                        }
                                        Err(e) => {
                                            debug!("TCP read: {e}");
                                            running.store(false, Ordering::SeqCst);
                                            break;
                                        }
                                    }
                                }
                                _ = &mut shutdown_a_rx => {
                                    debug!("Emulator→host task: shutdown");
                                    break;
                                }
                            }
                        }
                    });
                }

                // Task B: host → emulator (UHID read → TCP write)
                {
                    let tx = intercept_tx;
                    tokio::spawn(async move {
                        while let Some(report) = uhid_read_rx.recv().await {
                            let _ = tx.send(InterceptedMessage::new(
                                Direction::HostToDevice, &report, None,
                            ));
                            let payload = if !report.is_empty() && report[0] == 0x00 {
                                &report[1..]
                            } else {
                                &report
                            };
                            if let Err(e) = write_half.write_all(payload).await {
                                error!("TCP write to emulator failed: {e}");
                                break;
                            }
                        }
                        debug!("Host→emulator task exiting");
                    });
                }
            }
            BridgeTransport::Unix { path } => {
                let tx = intercept_tx;
                let path_clone = path.clone();

                tokio::spawn(async move {
                    loop {
                        let first_report = tokio::select! {
                            report = uhid_read_rx.recv() => report,
                            _ = &mut shutdown_a_rx => { return; }
                        };
                        let Some(first_report) = first_report else { return; };

                        debug!("Unix relay: connecting to {}", path_clone.display());
                        let stream = match tokio::net::UnixStream::connect(&path_clone).await {
                            Ok(s) => s,
                            Err(e) => {
                                error!("Unix relay: connect failed: {e}");
                                running.store(false, Ordering::SeqCst);
                                return;
                            }
                        };
                        let (mut read_half, mut write_half) = tokio::io::split(stream);

                        let payload = if !first_report.is_empty() && first_report[0] == 0x00 {
                            &first_report[1..]
                        } else {
                            &first_report
                        };
                        let _ = tx.send(InterceptedMessage::new(
                            Direction::HostToDevice, &first_report, None,
                        ));
                        if write_half.write_all(payload).await.is_err() {
                            continue;
                        }

                        let tx2 = tx.clone();
                        let running2 = Arc::clone(&running);
                        let uhid_write = uhid_write_tx.clone();
                        let read_task = tokio::spawn(async move {
                            let mut buf = vec![0u8; report_size];
                            loop {
                                match read_half.read_exact(&mut buf).await {
                                    Ok(_) => {
                                        let data = buf.clone();
                                        let _ = tx2.send(InterceptedMessage::new(
                                            Direction::DeviceToHost, &data, None,
                                        ));
                                        if uhid_write.send(data).is_err() {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        });

                        loop {
                            if read_task.is_finished() {
                                break;
                            }
                            tokio::select! {
                                report = uhid_read_rx.recv() => {
                                    let Some(report) = report else { return; };
                                    let _ = tx.send(InterceptedMessage::new(
                                        Direction::HostToDevice, &report, None,
                                    ));
                                    let payload = if !report.is_empty() && report[0] == 0x00 {
                                        &report[1..]
                                    } else {
                                        &report
                                    };
                                    if write_half.write_all(payload).await.is_err() {
                                        break;
                                    }
                                }
                                _ = &mut shutdown_a_rx => { return; }
                            }
                        }
                    }
                });
            }
        }

        Ok(intercept_rx)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        info!(name = %self.config.name, "Stopping GenericBridge");
        self.shutdown_tx.clear();
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uhid::{
        BITBOX02_HID_REPORT_DESCRIPTOR, BITBOX02_PID, BITBOX02_VID, COLDCARD_HID_REPORT_DESCRIPTOR,
        COLDCARD_PID, COLDCARD_VID, LEDGER_HID_REPORT_DESCRIPTOR, LEDGER_PID, LEDGER_VID,
    };

    #[test]
    fn config_builder_defaults() {
        let cfg = GenericBridgeConfig::new(
            BITBOX02_VID,
            BITBOX02_PID,
            "BitBox02",
            BITBOX02_HID_REPORT_DESCRIPTOR,
            BridgeTransport::Tcp {
                host: "127.0.0.1".into(),
                port: 15423,
            },
        );
        assert_eq!(cfg.vid, 0x03EB);
        assert_eq!(cfg.pid, 0x2403);
        assert_eq!(cfg.report_size, 64);
        assert_eq!(cfg.name, "BitBox02");
    }

    #[test]
    fn config_with_report_size() {
        let cfg = GenericBridgeConfig::new(
            LEDGER_VID,
            LEDGER_PID,
            "Ledger (emulated)",
            LEDGER_HID_REPORT_DESCRIPTOR,
            BridgeTransport::Tcp {
                host: "127.0.0.1".into(),
                port: 9999,
            },
        )
        .with_report_size(128);
        assert_eq!(cfg.report_size, 128);
    }

    #[test]
    fn config_unix_transport() {
        let cfg = GenericBridgeConfig::new(
            COLDCARD_VID,
            COLDCARD_PID,
            "Coldcard (emulated)",
            COLDCARD_HID_REPORT_DESCRIPTOR,
            BridgeTransport::Unix {
                path: PathBuf::from("/tmp/ckcc-simulator.sock"),
            },
        );
        assert!(matches!(cfg.transport, BridgeTransport::Unix { .. }));
    }

    #[test]
    fn bridge_not_running_initially() {
        let cfg = GenericBridgeConfig::new(
            BITBOX02_VID,
            BITBOX02_PID,
            "BitBox02",
            BITBOX02_HID_REPORT_DESCRIPTOR,
            BridgeTransport::Tcp {
                host: "127.0.0.1".into(),
                port: 15423,
            },
        );
        let bridge = GenericBridge::new(cfg);
        assert!(!bridge.is_running());
    }
}

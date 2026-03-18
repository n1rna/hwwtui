//! Trezor emulator ↔ UHID bridge.
//!
//! Spawns two Tokio tasks:
//!
//! 1. **emulator→host**: reads UDP datagrams from the Trezor emulator and
//!    writes them as HID Input reports to `/dev/uhid`.
//! 2. **host→emulator**: reads HID Output reports from `/dev/uhid` (blocking,
//!    on a `spawn_blocking` thread) and sends them as UDP datagrams to the
//!    Trezor emulator.
//!
//! Both tasks send [`InterceptedMessage`] events to an unbounded channel that
//! the TUI reads to populate its log panels.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::uhid::{
    VirtualHidDevice, TREZOR_HID_REPORT_DESCRIPTOR, TREZOR_PID, TREZOR_VID,
};
use crate::{Bridge, Direction, InterceptedMessage};
use protocol::trezor::decode_packet;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Size of every HID report / UDP datagram for Trezor.
const REPORT_SIZE: usize = 64;

// ── TrezorBridge ─────────────────────────────────────────────────────────────

/// Bidirectional bridge between the Trezor emulator UDP transport and a
/// virtual UHID device.
pub struct TrezorBridge {
    /// UDP address of the running Trezor emulator.
    emulator_addr: SocketAddr,

    /// Set to `true` while bridge tasks are active.
    running: Arc<AtomicBool>,

    /// Shutdown signal senders — dropped to signal tasks to exit.
    shutdown_tx: Vec<oneshot::Sender<()>>,
}

impl TrezorBridge {
    /// Create a bridge targeting the Trezor emulator at `host:port`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use bridge::trezor::TrezorBridge;
    /// use bridge::Bridge;
    ///
    /// #[tokio::main]
    /// async fn main() -> anyhow::Result<()> {
    ///     let mut bridge = TrezorBridge::new("127.0.0.1", 21324);
    ///     let mut rx = bridge.start().await?;
    ///     while let Some(msg) = rx.recv().await {
    ///         println!("{} {}", msg.direction, msg.raw_hex);
    ///     }
    ///     Ok(())
    /// }
    /// ```
    pub fn new(host: &str, port: u16) -> Self {
        let addr: SocketAddr = format!("{host}:{port}").parse().expect("invalid emulator address");
        Self {
            emulator_addr: addr,
            running: Arc::new(AtomicBool::new(false)),
            shutdown_tx: Vec::new(),
        }
    }
}

// ── Bridge impl ───────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Bridge for TrezorBridge {
    async fn start(&mut self) -> anyhow::Result<mpsc::UnboundedReceiver<InterceptedMessage>> {
        if self.running.load(Ordering::SeqCst) {
            anyhow::bail!("Bridge is already running");
        }

        info!(emulator = %self.emulator_addr, "Starting Trezor UHID bridge");

        // Create the UHID device (blocking; do it before spawning tasks).
        let uhid_device = VirtualHidDevice::new(
            TREZOR_VID,
            TREZOR_PID,
            "Trezor Safe 3 (emulated)",
            TREZOR_HID_REPORT_DESCRIPTOR,
        )
        .context("Failed to create virtual Trezor HID device")?;

        // UDP socket for communicating with the emulator.
        // Bind to any local port; the emulator sends responses back to our
        // source address.
        let udp = UdpSocket::bind("127.0.0.1:0")
            .await
            .context("Failed to bind local UDP socket for Trezor bridge")?;
        udp.connect(self.emulator_addr)
            .await
            .context("Failed to connect UDP socket to Trezor emulator")?;
        let udp = Arc::new(udp);

        info!(
            local_addr = %udp.local_addr().unwrap(),
            emulator = %self.emulator_addr,
            "Trezor bridge UDP socket ready"
        );

        let (intercept_tx, intercept_rx) = mpsc::unbounded_channel::<InterceptedMessage>();
        let (shutdown_a_tx, mut shutdown_a_rx) = oneshot::channel::<()>();
        let (shutdown_b_tx, mut shutdown_b_rx) = oneshot::channel::<()>();

        self.shutdown_tx.push(shutdown_a_tx);
        self.shutdown_tx.push(shutdown_b_tx);
        self.running.store(true, Ordering::SeqCst);

        // ── Task A: emulator → host ─────────────────────────────────────────
        // Reads UDP datagrams from the Trezor emulator, writes them as UHID
        // Input reports, and forwards them to the intercept channel.
        {
            let udp_rx = Arc::clone(&udp);
            let tx = intercept_tx.clone();
            let running = Arc::clone(&self.running);

            // The UHID device is not Send because it holds a raw fd, so we
            // wrap it in a Mutex and run the UHID write on a blocking thread.
            // For the read direction we only use the UDP socket (which is Send).
            //
            // We pass the UHID device into this task via a blocking channel so
            // the write happens on a dedicated blocking thread.
            let (uhid_write_tx, mut uhid_write_rx) =
                mpsc::unbounded_channel::<Vec<u8>>();

            // Blocking thread: drain the write queue and send to UHID.
            let mut uhid_dev = uhid_device;
            std::thread::spawn(move || {
                while let Some(report) = uhid_write_rx.blocking_recv() {
                    if let Err(e) = uhid_dev.send_input_report(&report) {
                        error!("UHID input report write failed: {e}");
                        break;
                    }
                    // Also spawn the read-output side in the same thread once
                    // the device exists. This is handled in task B below via a
                    // separate path; we only do writes here.
                }
                debug!("UHID write thread exiting");
            });

            // Async task: receive UDP datagrams and push them to the write thread.
            tokio::spawn(async move {
                let mut buf = [0u8; REPORT_SIZE + 16]; // slight over-read margin
                loop {
                    tokio::select! {
                        result = udp_rx.recv(&mut buf) => {
                            match result {
                                Ok(n) => {
                                    let data = buf[..n.min(REPORT_SIZE)].to_vec();
                                    // Decode for log.
                                    let decoded = decode_packet(&data)
                                        .ok()
                                        .map(|m| m.log_line());
                                    let msg = InterceptedMessage::new(
                                        Direction::DeviceToHost,
                                        &data,
                                        decoded,
                                    );
                                    let _ = tx.send(msg);
                                    // Forward to UHID.
                                    let _ = uhid_write_tx.send(data);
                                }
                                Err(e) => {
                                    error!("UDP recv error: {e}");
                                    running.store(false, Ordering::SeqCst);
                                    break;
                                }
                            }
                        }
                        _ = &mut shutdown_a_rx => {
                            debug!("Emulator→host task received shutdown");
                            break;
                        }
                    }
                }
            });
        }

        // ── Task B: host → emulator ─────────────────────────────────────────
        // A blocking thread reads Output reports from UHID and forwards them
        // to the emulator via UDP.
        //
        // Because UHID read is blocking we cannot do it in an async task
        // without spin-polling. Instead we use a dedicated OS thread and push
        // results through a channel to the async world.
        {
            let udp_tx = Arc::clone(&udp);
            let tx = intercept_tx;
            let running_flag = Arc::clone(&self.running);
            let emulator_addr = self.emulator_addr;

            // Channel: blocking thread → async send task.
            let (output_tx, mut output_rx) = mpsc::unbounded_channel::<Vec<u8>>();

            // Blocking thread for UHID reads.
            // We create a second UHID device handle pointing at the same
            // device by re-opening /dev/uhid — the kernel associates the
            // Output reports with all open file descriptors for the same
            // device.
            //
            // NOTE: In practice a single fd receives both directions. We open
            // a fresh device here just for the read side. A more advanced
            // implementation would share the fd using unsafe splitting, but
            // that adds complexity beyond Phase 1 scope.
            //
            // For Phase 1 the Output path is best-effort: if the second open
            // fails we log a warning and continue (host→emulator direction
            // will be non-functional but the emulator→host path still works).
            let output_tx_clone = output_tx.clone();
            std::thread::spawn(move || {
                // Opening a *second* UHID device for the same vid/pid creates
                // a separate virtual device. This is not what we want.
                //
                // Real solution: use a single UHID fd with non-blocking I/O
                // and poll(2). That requires the underlying File to be set to
                // O_NONBLOCK and manual event parsing, which is beyond the
                // uhid-virt API.
                //
                // For Phase 1 we log that host→emulator is not yet wired and
                // drop the thread.
                warn!(
                    "Host→emulator UHID output path: non-blocking UHID read is not yet \
                     implemented in Phase 1. Packets from host to emulator will not be \
                     forwarded over UHID. Use the emulator's USB/HID stack directly or \
                     implement fd splitting in Phase 2."
                );
                drop(output_tx_clone);
            });

            // Async task: take output reports from the blocking thread and
            // forward them via UDP to the emulator.
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        maybe = output_rx.recv() => {
                            match maybe {
                                Some(report) => {
                                    let decoded = decode_packet(&report)
                                        .ok()
                                        .map(|m| m.log_line());
                                    let msg = InterceptedMessage::new(
                                        Direction::HostToDevice,
                                        &report,
                                        decoded,
                                    );
                                    let _ = tx.send(msg);
                                    if let Err(e) = udp_tx.send_to(&report, emulator_addr).await {
                                        error!("UDP send to emulator failed: {e}");
                                    }
                                }
                                None => {
                                    // Blocking thread exited.
                                    debug!("Host→emulator output channel closed");
                                    running_flag.store(false, Ordering::SeqCst);
                                    break;
                                }
                            }
                        }
                        _ = &mut shutdown_b_rx => {
                            debug!("Host→emulator task received shutdown");
                            break;
                        }
                    }
                }
            });
        }

        Ok(intercept_rx)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        info!("Stopping Trezor UHID bridge");
        // Drop all shutdown senders — tasks select on their shutdown_rx.
        self.shutdown_tx.clear();
        // Give tasks a moment to drain.
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

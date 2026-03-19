//! Trezor emulator ↔ UHID bridge.
//!
//! Uses a single UHID device on a dedicated blocking thread that handles
//! both directions:
//! - **emulator→host**: receives write requests via channel, sends as HID Input reports
//! - **host→emulator**: polls for Output reports (non-blocking), sends back via channel

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info};

use crate::uhid::{VirtualHidDevice, TREZOR_HID_REPORT_DESCRIPTOR, TREZOR_PID, TREZOR_VID};
use crate::{Bridge, Direction, InterceptedMessage};
use protocol::trezor::decode_packet;

/// Size of every HID report / UDP datagram for Trezor.
const REPORT_SIZE: usize = 64;

pub struct TrezorBridge {
    emulator_addr: SocketAddr,
    running: Arc<AtomicBool>,
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
        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .expect("invalid emulator address");
        Self {
            emulator_addr: addr,
            running: Arc::new(AtomicBool::new(false)),
            shutdown_tx: Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl Bridge for TrezorBridge {
    async fn start(&mut self) -> anyhow::Result<mpsc::UnboundedReceiver<InterceptedMessage>> {
        if self.running.load(Ordering::SeqCst) {
            anyhow::bail!("Bridge is already running");
        }

        info!(emulator = %self.emulator_addr, "Starting Trezor UHID bridge");

        let uhid_device = VirtualHidDevice::new(
            TREZOR_VID,
            TREZOR_PID,
            "Trezor Safe 3 (emulated)",
            TREZOR_HID_REPORT_DESCRIPTOR,
        )
        .context("Failed to create virtual Trezor HID device")?;

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
        let (shutdown_b_tx, shutdown_b_rx) = oneshot::channel::<()>();

        self.shutdown_tx.push(shutdown_a_tx);
        self.shutdown_tx.push(shutdown_b_tx);
        self.running.store(true, Ordering::SeqCst);

        // Channels for the UHID blocking thread:
        //   uhid_write_tx -> blocking thread reads and writes Input reports to UHID
        //   uhid_read_tx  <- blocking thread polls Output reports and sends them back
        let (uhid_write_tx, uhid_write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (uhid_read_tx, mut uhid_read_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        // Single blocking thread that owns the UHID device for both directions.
        Self::spawn_uhid_thread(uhid_device, uhid_write_rx, uhid_read_tx, shutdown_b_rx);

        // Task A: emulator → host (UDP recv → UHID write via channel)
        {
            let udp_rx = Arc::clone(&udp);
            let tx = intercept_tx.clone();
            let running = Arc::clone(&self.running);

            tokio::spawn(async move {
                let mut buf = [0u8; REPORT_SIZE + 16];
                loop {
                    tokio::select! {
                        result = udp_rx.recv(&mut buf) => {
                            match result {
                                Ok(n) => {
                                    let data = buf[..n.min(REPORT_SIZE)].to_vec();
                                    let decoded = decode_packet(&data).ok().map(|m| m.log_line());
                                    let msg = InterceptedMessage::new(
                                        Direction::DeviceToHost, &data, decoded,
                                    );
                                    let _ = tx.send(msg);
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

        // Task B: host → emulator (UHID read via channel → UDP send)
        {
            let udp_tx = Arc::clone(&udp);
            let tx = intercept_tx;
            let emulator_addr = self.emulator_addr;

            tokio::spawn(async move {
                while let Some(report) = uhid_read_rx.recv().await {
                    let decoded = decode_packet(&report).ok().map(|m| m.log_line());
                    let msg = InterceptedMessage::new(Direction::HostToDevice, &report, decoded);
                    let _ = tx.send(msg);
                    if let Err(e) = udp_tx.send_to(&report, emulator_addr).await {
                        error!("UDP send to emulator failed: {e}");
                    }
                }
                debug!("Host→emulator task exiting (UHID read channel closed)");
            });
        }

        Ok(intercept_rx)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        info!("Stopping Trezor UHID bridge");
        self.shutdown_tx.clear();
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl TrezorBridge {
    /// Spawn a blocking thread that owns the UHID device and handles both
    /// read and write directions.
    fn spawn_uhid_thread(
        mut uhid_dev: VirtualHidDevice,
        mut write_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        read_tx: mpsc::UnboundedSender<Vec<u8>>,
        mut shutdown_rx: oneshot::Receiver<()>,
    ) {
        std::thread::spawn(move || {
            debug!("UHID bidirectional thread started");
            loop {
                // Check for shutdown.
                if shutdown_rx.try_recv().is_ok() {
                    debug!("UHID thread received shutdown");
                    break;
                }

                // Write: drain any pending input reports (emulator → host).
                while let Ok(report) = write_rx.try_recv() {
                    if let Err(e) = uhid_dev.send_input_report(&report) {
                        error!("UHID input report write failed: {e}");
                        return;
                    }
                }

                // Read: poll for output reports (host → emulator).
                match uhid_dev.poll_output_report() {
                    Ok(Some(data)) => {
                        debug!(len = data.len(), "UHID output report from host");
                        if read_tx.send(data).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Ok(None) => {} // no event
                    Err(e) => {
                        error!("UHID poll error: {e}");
                        break;
                    }
                }

                // Small sleep to avoid busy-spinning.
                std::thread::sleep(Duration::from_millis(1));
            }
            debug!("UHID bidirectional thread exiting");
        });
    }
}

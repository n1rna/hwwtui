//! UHID bridge layer.
//!
//! The bridge connects an emulator's native transport (UDP, TCP, Unix socket)
//! to a virtual HID device exposed to the host OS through `/dev/uhid`.
//!
//! ```text
//!  ┌──────────────────────────────────────────────────────┐
//!  │  Host OS                                             │
//!  │  (trezorctl / python-trezor / sigvault-desktop)      │
//!  │              │  /dev/hidraw0                         │
//!  └──────────────┼───────────────────────────────────────┘
//!                 │  HID reports (64 bytes)
//!  ┌──────────────┼───────────────────────────────────────┐
//!  │  hwwtui      │  /dev/uhid  (UHID kernel module)      │
//!  │         ┌────┴────────────────┐                      │
//!  │         │  VirtualHidDevice   │  uhid-virt           │
//!  │         └────┬───────────────┬┘                      │
//!  │              │               │                       │
//!  │         Output            Input                      │
//!  │         (host→dev)        (dev→host)                 │
//!  │              │               │                       │
//!  │         ┌────┴───────────────┴────┐                  │
//!  │         │  TrezorBridge           │                  │
//!  │         │  UDP socket             │                  │
//!  │         └────────────────────────┘                  │
//!  └──────────────────────────────────────────────────────┘
//!                 │  UDP datagrams (64 bytes)
//!  ┌──────────────┼───────────────────────────────────────┐
//!  │  Trezor emulator process                             │
//!  └──────────────────────────────────────────────────────┘
//! ```
//!
//! # Permissions
//!
//! Writing to `/dev/uhid` requires either `root` or membership in the
//! `uhid` or `plugdev` group, depending on your udev rules. A suitable rule:
//!
//! ```text
//! KERNEL=="uhid", MODE="0660", GROUP="plugdev"
//! ```

pub mod generic;
pub mod trezor;
pub mod uhid;

use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::mpsc;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A raw HID report (always 64 bytes for Trezor-class devices).
#[derive(Debug, Clone)]
pub struct HidReport {
    pub data: Vec<u8>,
    pub timestamp: Instant,
}

impl HidReport {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            timestamp: Instant::now(),
        }
    }
}

/// Direction of a bridged message, from the perspective of the virtual device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Host application → virtual device (Output HID report).
    HostToDevice,
    /// Virtual device → host application (Input HID report).
    DeviceToHost,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostToDevice => write!(f, ">>"),
            Self::DeviceToHost => write!(f, "<<"),
        }
    }
}

/// A packet that has been observed crossing the bridge, with optional decode.
#[derive(Debug, Clone)]
pub struct InterceptedMessage {
    pub direction: Direction,
    /// Raw bytes as hex string, e.g. `"3f 23 23 00 00 ..."`
    pub raw_hex: String,
    /// Human-readable decode if the protocol module could parse the header.
    pub decoded: Option<String>,
    pub timestamp: Instant,
}

impl InterceptedMessage {
    pub fn new(direction: Direction, data: &[u8], decoded: Option<String>) -> Self {
        let raw_hex = data
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            direction,
            raw_hex,
            decoded,
            timestamp: Instant::now(),
        }
    }
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Common interface for all emulator↔UHID bridges.
#[async_trait]
pub trait Bridge: Send + Sync {
    /// Start the bridge task.
    ///
    /// Returns a channel receiver through which all intercepted messages are
    /// sent. The bridge runs in background Tokio tasks until [`stop`] is called
    /// or the emulator transport closes.
    async fn start(&mut self) -> anyhow::Result<mpsc::UnboundedReceiver<InterceptedMessage>>;

    /// Gracefully stop the bridge and close the UHID device.
    async fn stop(&mut self) -> anyhow::Result<()>;

    /// Whether the bridge tasks are currently running.
    fn is_running(&self) -> bool;
}

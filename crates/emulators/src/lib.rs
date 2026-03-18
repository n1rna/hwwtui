//! Emulator process management for hardware wallet simulators.
//!
//! Each emulator variant wraps a child process (or Docker container for later
//! additions) and exposes a uniform [`Emulator`] trait so the TUI can manage
//! all devices the same way.

pub mod trezor;

use std::path::PathBuf;

use async_trait::async_trait;

// ── Status ────────────────────────────────────────────────────────────────────

/// Lifecycle state of an emulator process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmulatorStatus {
    /// Process has not been started or has been stopped cleanly.
    Stopped,
    /// Process start has been requested; waiting for the transport to become ready.
    Starting,
    /// Process is running and the transport is reachable.
    Running,
    /// Process died or failed health check.
    Error(String),
}

impl std::fmt::Display for EmulatorStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => write!(f, "Stopped"),
            Self::Starting => write!(f, "Starting"),
            Self::Running => write!(f, "Running"),
            Self::Error(msg) => write!(f, "Error: {msg}"),
        }
    }
}

// ── Wallet types ──────────────────────────────────────────────────────────────

/// Identifies which hardware wallet model an emulator represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WalletType {
    Trezor,
    #[serde(rename = "bitbox02")]
    BitBox02,
    Coldcard,
    Specter,
    Ledger,
    Jade,
}

impl std::fmt::Display for WalletType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trezor => write!(f, "Trezor"),
            Self::BitBox02 => write!(f, "BitBox02"),
            Self::Coldcard => write!(f, "Coldcard"),
            Self::Specter => write!(f, "Specter"),
            Self::Ledger => write!(f, "Ledger"),
            Self::Jade => write!(f, "Jade"),
        }
    }
}

// ── Transport ─────────────────────────────────────────────────────────────────

/// How the bridge layer talks to a running emulator.
#[derive(Debug, Clone)]
pub enum TransportConfig {
    /// Trezor emulator: UDP datagrams on localhost.
    Udp { host: String, port: u16 },
    /// TCP socket (e.g. Coldcard emulator).
    Tcp { host: String, port: u16 },
    /// Unix domain socket (e.g. some BitBox02 builds).
    UnixSocket { path: PathBuf },
}

impl std::fmt::Display for TransportConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Udp { host, port } => write!(f, "UDP {host}:{port}"),
            Self::Tcp { host, port } => write!(f, "TCP {host}:{port}"),
            Self::UnixSocket { path } => write!(f, "Unix {}", path.display()),
        }
    }
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Common interface for all hardware wallet emulators.
#[async_trait]
pub trait Emulator: Send + Sync {
    /// Which wallet model this emulator represents.
    fn wallet_type(&self) -> WalletType;

    /// Current lifecycle status.
    fn status(&self) -> EmulatorStatus;

    /// How to reach this emulator from the bridge.
    fn transport(&self) -> TransportConfig;

    /// Spawn the emulator process (or container). Sets status to Starting,
    /// then waits for the transport to become reachable before setting Running.
    async fn start(&mut self) -> anyhow::Result<()>;

    /// Gracefully terminate the emulator. Sets status to Stopped.
    async fn stop(&mut self) -> anyhow::Result<()>;

    /// Returns `true` if the transport endpoint is currently reachable.
    async fn health_check(&self) -> bool;
}

//! Application configuration.
//!
//! For Phase 1, configuration is minimal and comes from environment variables
//! with compile-time defaults. A full CLI / config-file story can be added
//! in Phase 2.

use std::path::PathBuf;

/// Top-level application configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Wallet-specific device configurations.
    pub devices: Vec<DeviceConfig>,
    /// GitHub repository for bundle downloads, e.g. `"hwwtui/hwwtui"`.
    pub github_repo: String,
}

/// Per-device configuration.
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    pub kind: DeviceKind,
    pub enabled: bool,
    pub trezor: Option<TrezorConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Trezor,
    BitBox02,
    Coldcard,
    Specter,
    Ledger,
    Jade,
}

impl std::fmt::Display for DeviceKind {
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

/// Configuration specific to the Trezor emulator.
#[derive(Debug, Clone)]
pub struct TrezorConfig {
    /// Path to `trezor-firmware/core/` directory.
    /// If `None`, the bundler path will be used at runtime.
    pub firmware_path: Option<PathBuf>,
    /// Directory for flash/SD-card simulation state.
    pub profile_dir: PathBuf,
    /// UDP port the emulator will listen on.
    pub port: u16,
}

impl Config {
    /// Build configuration from environment variables, falling back to defaults.
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `HWWTUI_GITHUB_REPO` | `"n1rna/hwwtui"` |
    /// | `TREZOR_FIRMWARE_PATH` | `None` (uses bundler path) |
    /// | `TREZOR_PROFILE_DIR` | `/tmp/hwwtui-trezor` |
    /// | `TREZOR_PORT` | `21324` |
    pub fn from_env_or_defaults() -> Self {
        let github_repo =
            std::env::var("HWWTUI_GITHUB_REPO").unwrap_or_else(|_| "n1rna/hwwtui".to_string());

        let trezor_cfg = TrezorConfig {
            firmware_path: std::env::var("TREZOR_FIRMWARE_PATH")
                .ok()
                .map(PathBuf::from),
            profile_dir: std::env::var("TREZOR_PROFILE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/tmp/hwwtui-trezor")),
            port: std::env::var("TREZOR_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(21324),
        };

        let all_kinds = [
            DeviceKind::Trezor,
            DeviceKind::BitBox02,
            DeviceKind::Coldcard,
            DeviceKind::Specter,
            DeviceKind::Ledger,
            DeviceKind::Jade,
        ];

        let devices = all_kinds
            .into_iter()
            .map(|kind| DeviceConfig {
                kind,
                // All devices are usable once their bundle is downloaded.
                enabled: true,
                trezor: if kind == DeviceKind::Trezor {
                    Some(trezor_cfg.clone())
                } else {
                    None
                },
            })
            .collect();

        Self {
            devices,
            github_repo,
        }
    }
}

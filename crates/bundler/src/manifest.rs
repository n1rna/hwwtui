//! Bundle manifest — metadata stored alongside each downloaded bundle.

use emulators::WalletType;
use serde::{Deserialize, Serialize};

/// Manifest stored alongside each downloaded bundle in `manifest.json`.
///
/// The manifest is written atomically after a successful extraction and
/// serves as the source of truth for what is installed on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    /// Which hardware wallet this bundle is for.
    pub wallet_type: WalletType,
    /// Upstream release version, e.g. `"v2.10.0"`.
    pub version: String,
    /// Target platform string, e.g. `"linux-x86_64"`.
    pub platform: String,
    /// ISO 8601 timestamp of when this bundle was installed.
    pub installed_at: String,
    /// Total on-disk size of the extracted bundle in bytes.
    pub size_bytes: u64,
    /// Relative path (from the bundle dir) to the main emulator executable.
    ///
    /// Example: `"trezor-emu-core"` or `"bin/trezord"`.
    pub emulator_binary: String,
    /// Optional relative path (from the bundle dir) to a firmware/data
    /// directory that the emulator reads at runtime.
    pub firmware_dir: Option<String>,
    /// Arbitrary extra metadata shipped with the build (e.g. git commit,
    /// build flags).  May be `null` when absent.
    pub build_info: Option<serde_json::Value>,
}

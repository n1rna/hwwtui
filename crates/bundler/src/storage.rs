//! Local filesystem storage for downloaded bundles.
//!
//! All bundles live under `~/.hwwtui/bundles/{wallet}/`.  Each wallet
//! directory contains a `manifest.json` and the extracted bundle contents.

use std::path::{Path, PathBuf};

use anyhow::Context;
use emulators::WalletType;

use crate::manifest::BundleManifest;

/// Manages the on-disk layout of downloaded bundles.
///
/// # Layout
///
/// ```text
/// ~/.hwwtui/
/// └── bundles/
///     ├── trezor/
///     │   ├── manifest.json
///     │   ├── trezor-emu-core
///     │   └── data/
///     ├── bitbox02/
///     │   ├── manifest.json
///     │   └── ...
///     └── ...
/// ```
pub struct BundleStorage {
    /// Root bundles directory: `~/.hwwtui/bundles`.
    base_dir: PathBuf,
}

impl BundleStorage {
    /// Create a new `BundleStorage`, ensuring the base directory exists.
    ///
    /// The base directory is `~/.hwwtui/bundles`.  It is created (with all
    /// parent directories) if it does not yet exist.
    pub fn new() -> anyhow::Result<Self> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        let base_dir = home.join(".hwwtui").join("bundles");
        std::fs::create_dir_all(&base_dir)
            .with_context(|| format!("failed to create bundles dir: {}", base_dir.display()))?;
        Ok(Self { base_dir })
    }

    /// Returns `~/.hwwtui/bundles`.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Returns the directory for a specific wallet type.
    ///
    /// Example: `~/.hwwtui/bundles/trezor/`
    pub fn bundle_dir(&self, wallet_type: WalletType) -> PathBuf {
        self.base_dir.join(wallet_dir_name(wallet_type))
    }

    /// Returns the path to `manifest.json` for the given wallet type.
    pub fn manifest_path(&self, wallet_type: WalletType) -> PathBuf {
        self.bundle_dir(wallet_type).join("manifest.json")
    }

    /// Read and deserialize the manifest for a wallet type.
    ///
    /// Returns `None` if the manifest file does not exist or cannot be parsed.
    pub fn read_manifest(&self, wallet_type: WalletType) -> Option<BundleManifest> {
        let path = self.manifest_path(wallet_type);
        let data = std::fs::read(&path).ok()?;
        serde_json::from_slice(&data)
            .map_err(|e| {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to parse bundle manifest"
                );
                e
            })
            .ok()
    }

    /// Serialize and write a manifest to disk.
    ///
    /// The bundle directory is created if it does not exist.
    pub fn write_manifest(
        &self,
        wallet_type: WalletType,
        manifest: &BundleManifest,
    ) -> anyhow::Result<()> {
        let dir = self.bundle_dir(wallet_type);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create bundle dir: {}", dir.display()))?;

        let path = self.manifest_path(wallet_type);
        let json = serde_json::to_string_pretty(manifest).context("failed to serialize manifest")?;

        // Write to a temp file in the same directory then rename for atomicity.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json.as_bytes())
            .with_context(|| format!("failed to write tmp manifest: {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("failed to rename manifest into place: {}", path.display()))?;

        tracing::debug!(path = %path.display(), "manifest written");
        Ok(())
    }

    /// Delete the entire bundle directory for a wallet type.
    ///
    /// This is a no-op (returns `Ok(())`) if the directory does not exist.
    pub fn remove_bundle(&self, wallet_type: WalletType) -> anyhow::Result<()> {
        let dir = self.bundle_dir(wallet_type);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("failed to remove bundle dir: {}", dir.display()))?;
            tracing::info!(dir = %dir.display(), "bundle removed");
        } else {
            tracing::debug!(dir = %dir.display(), "remove_bundle: directory does not exist, nothing to do");
        }
        Ok(())
    }
}

/// Convert a `WalletType` to the lowercase directory name used on disk.
fn wallet_dir_name(wallet_type: WalletType) -> &'static str {
    match wallet_type {
        WalletType::Trezor => "trezor",
        WalletType::BitBox02 => "bitbox02",
        WalletType::Coldcard => "coldcard",
        WalletType::Specter => "specter",
        WalletType::Ledger => "ledger",
        WalletType::Jade => "jade",
    }
}

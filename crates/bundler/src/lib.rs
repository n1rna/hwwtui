//! `bundler` — download, store, and manage pre-built emulator firmware bundles
//! fetched from GitHub Releases.
//!
//! # Quick start
//!
//! ```no_run
//! use bundler::BundleManager;
//! use emulators::WalletType;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let manager = BundleManager::new("myorg/hwwtui")?;
//!
//!     // List bundles available on GitHub for the current platform.
//!     let remote = manager.list_remote().await?;
//!     for bundle in &remote {
//!         println!("{:?} {} ({})", bundle.wallet_type, bundle.version, bundle.platform);
//!     }
//!
//!     // Download the first one with live progress reporting.
//!     if let Some(bundle) = remote.first() {
//!         let (tx, mut rx) = tokio::sync::watch::channel(bundler::BundleStatus::NotInstalled);
//!         tokio::spawn(async move {
//!             while rx.changed().await.is_ok() {
//!                 println!("status: {:?}", *rx.borrow());
//!             }
//!         });
//!         manager.download(bundle, tx).await?;
//!     }
//!
//!     // Check installation status.
//!     println!("{:?}", manager.status(WalletType::Trezor));
//!     Ok(())
//! }
//! ```

pub mod download;
pub mod manifest;
pub mod storage;

use std::path::PathBuf;

use emulators::WalletType;
use serde::{Deserialize, Serialize};

pub use manifest::BundleManifest;

// ── Public types ──────────────────────────────────────────────────────────────

/// Identifies a specific firmware bundle (wallet + version + platform).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleId {
    pub wallet_type: WalletType,
    /// Upstream release tag, e.g. `"v2.10.0"`.
    pub version: String,
    /// Target platform: `"linux-x86_64"`, `"linux-aarch64"`, `"macos-x86_64"`,
    /// `"macos-aarch64"`.
    pub platform: String,
}

/// Installation status of a bundle on the local machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BundleStatus {
    /// The bundle has not been downloaded yet.
    NotInstalled,
    /// A download is currently in progress.  `progress_pct` is in `0..=100`.
    Downloading { progress_pct: u8 },
    /// The bundle is on disk and ready to use.
    Installed { version: String, size_bytes: u64 },
    /// The last download or extraction attempt failed.
    Failed { error: String },
}

/// Metadata about a bundle asset that is available for download on GitHub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteBundle {
    pub wallet_type: WalletType,
    /// Release tag, e.g. `"v2.10.0"`.
    pub version: String,
    /// Target platform string, e.g. `"linux-x86_64"`.
    pub platform: String,
    /// HTTPS URL pointing at the `.tar.gz` asset.
    pub download_url: String,
    /// Compressed asset size in bytes as reported by GitHub.
    pub size_bytes: u64,
    /// Original asset filename, e.g. `"hwwtui-trezor-linux-x86_64.tar.gz"`.
    pub asset_name: String,
}

// ── BundleManager ─────────────────────────────────────────────────────────────

/// Central façade for the bundle subsystem.
///
/// Combines [`storage::BundleStorage`] (local disk) with
/// [`download::GithubDownloader`] (remote GitHub Releases) and exposes a
/// clean async API consumed by the TUI.
pub struct BundleManager {
    storage: storage::BundleStorage,
    downloader: download::GithubDownloader,
}

impl BundleManager {
    /// Create a new `BundleManager` targeting the given GitHub repository.
    ///
    /// `github_repo` should be `"owner/repo"`, e.g. `"myorg/hwwtui"`.
    ///
    /// This call is synchronous and only initialises the local storage
    /// directory.  No network requests are made.
    pub fn new(github_repo: &str) -> anyhow::Result<Self> {
        let storage = storage::BundleStorage::new()?;
        let downloader = download::GithubDownloader::new(github_repo);
        Ok(Self {
            storage,
            downloader,
        })
    }

    // ── Remote queries ────────────────────────────────────────────────────────

    /// Fetch the latest GitHub release and return all bundles that match the
    /// current platform.
    ///
    /// Bundles for other platforms are filtered out so the caller only sees
    /// what is actually usable on this machine.
    pub async fn list_remote(&self) -> anyhow::Result<Vec<RemoteBundle>> {
        let platform = download::current_platform();
        let all = self.downloader.fetch_available().await?;
        let filtered = all
            .into_iter()
            .filter(|b| b.platform == platform)
            .collect();
        Ok(filtered)
    }

    // ── Local queries ─────────────────────────────────────────────────────────

    /// Return the current installation status for the given wallet type.
    pub fn status(&self, wallet_type: WalletType) -> BundleStatus {
        match self.storage.read_manifest(wallet_type) {
            Some(m) => BundleStatus::Installed {
                version: m.version,
                size_bytes: m.size_bytes,
            },
            None => BundleStatus::NotInstalled,
        }
    }

    /// Return the path to the main emulator binary for `wallet_type`, if it
    /// is installed.
    pub fn emulator_binary_path(&self, wallet_type: WalletType) -> Option<PathBuf> {
        let manifest = self.storage.read_manifest(wallet_type)?;
        Some(
            self.storage
                .bundle_dir(wallet_type)
                .join(&manifest.emulator_binary),
        )
    }

    /// Return the path to the firmware/data directory for `wallet_type`, if
    /// it is installed and the manifest specifies one.
    pub fn firmware_dir(&self, wallet_type: WalletType) -> Option<PathBuf> {
        let manifest = self.storage.read_manifest(wallet_type)?;
        let rel = manifest.firmware_dir?;
        Some(self.storage.bundle_dir(wallet_type).join(rel))
    }

    // ── Mutations ─────────────────────────────────────────────────────────────

    /// Download `bundle`, extract it, and write the manifest to disk.
    ///
    /// Progress is streamed through `progress_tx`:
    /// - [`BundleStatus::Downloading`] while bytes are being transferred.
    /// - [`BundleStatus::Installed`] on success.
    /// - [`BundleStatus::Failed`] if any step errors.
    ///
    /// On failure the partially-downloaded archive (if any) is cleaned up but
    /// previously installed content for this wallet type is left untouched.
    pub async fn download(
        &self,
        bundle: &RemoteBundle,
        progress_tx: tokio::sync::watch::Sender<BundleStatus>,
    ) -> anyhow::Result<()> {
        use anyhow::Context as _;

        let bundle_dir = self.storage.bundle_dir(bundle.wallet_type);

        // Ensure the bundle directory exists before we start.
        std::fs::create_dir_all(&bundle_dir).with_context(|| {
            format!("failed to create bundle dir: {}", bundle_dir.display())
        })?;

        // The tarball lands in the bundle dir temporarily.
        let archive_path = bundle_dir.join(&bundle.asset_name);

        // --- Step 1: stream download -----------------------------------------
        if let Err(e) = self
            .downloader
            .download_asset(&bundle.download_url, &archive_path, &progress_tx)
            .await
        {
            let msg = e.to_string();
            let _ = progress_tx.send(BundleStatus::Failed { error: msg.clone() });
            // Clean up the (possibly partial) archive file.
            let _ = std::fs::remove_file(&archive_path);
            return Err(e.context("bundle download failed"));
        }

        // --- Step 2: extract -------------------------------------------------
        let _ = progress_tx.send(BundleStatus::Downloading { progress_pct: 100 });

        if let Err(e) = download::extract_tarball(&archive_path, &bundle_dir).await {
            let msg = e.to_string();
            let _ = progress_tx.send(BundleStatus::Failed { error: msg.clone() });
            let _ = std::fs::remove_file(&archive_path);
            return Err(e.context("bundle extraction failed"));
        }

        // Remove the archive now that it is unpacked.
        let _ = std::fs::remove_file(&archive_path);

        // --- Step 3: discover the emulator binary ----------------------------
        let emulator_binary = find_emulator_binary(&bundle_dir, bundle.wallet_type);

        // --- Step 4: compute installed size ----------------------------------
        let size_bytes = dir_size(&bundle_dir).unwrap_or(0);

        // --- Step 5: write manifest ------------------------------------------
        let installed_at = current_timestamp();

        let manifest = BundleManifest {
            wallet_type: bundle.wallet_type,
            version: bundle.version.clone(),
            platform: bundle.platform.clone(),
            installed_at,
            size_bytes,
            emulator_binary,
            firmware_dir: detect_firmware_dir(&bundle_dir),
            build_info: None,
        };

        if let Err(e) = self.storage.write_manifest(bundle.wallet_type, &manifest) {
            let msg = e.to_string();
            let _ = progress_tx.send(BundleStatus::Failed { error: msg.clone() });
            return Err(e.context("failed to write bundle manifest"));
        }

        let _ = progress_tx.send(BundleStatus::Installed {
            version: bundle.version.clone(),
            size_bytes,
        });

        tracing::info!(
            wallet = %bundle.wallet_type,
            version = %bundle.version,
            "bundle installed successfully"
        );

        Ok(())
    }

    /// Remove the bundle for `wallet_type` from disk.
    ///
    /// Returns `Ok(())` if the bundle is not installed.
    pub fn remove(&self, wallet_type: WalletType) -> anyhow::Result<()> {
        self.storage.remove_bundle(wallet_type)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Attempt to find the main emulator executable inside `bundle_dir`.
///
/// The heuristic checks a set of known binary names for each wallet type and
/// falls back to the first executable file found in the top level of the
/// directory.  Returns a relative path string.
fn find_emulator_binary(bundle_dir: &std::path::Path, wallet_type: WalletType) -> String {
    // Known binary names ordered by preference.
    let candidates: &[&str] = match wallet_type {
        WalletType::Trezor => &["trezor-emu-core", "trezord-go", "trezord"],
        WalletType::BitBox02 => &["bitbox02-emu", "bitbox02-simulator"],
        WalletType::Coldcard => &["coldcard-emu", "coldcard"],
        WalletType::Specter => &["specter-emu", "specter"],
        WalletType::Ledger => &["ledger-emu", "nanos-secure-sdk", "ledger"],
        WalletType::Jade => &["jade-emu", "jade"],
    };

    for name in candidates {
        let path = bundle_dir.join(name);
        if path.exists() {
            return name.to_string();
        }
    }

    // Fallback: scan the top level for any executable file.
    if let Ok(rd) = std::fs::read_dir(bundle_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file() && is_executable(&p) {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    return name.to_string();
                }
            }
        }
    }

    // Final fallback: just use the wallet name.
    wallet_type.to_string().to_lowercase()
}

/// Detect an optional `data/` or `firmware/` subdirectory.
fn detect_firmware_dir(bundle_dir: &std::path::Path) -> Option<String> {
    for name in &["data", "firmware", "flash"] {
        if bundle_dir.join(name).is_dir() {
            return Some(name.to_string());
        }
    }
    None
}

/// Recursively sum the size of all files under `dir`.
fn dir_size(dir: &std::path::Path) -> anyhow::Result<u64> {
    let mut total = 0u64;
    for entry in walkdir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Minimal recursive directory walker (avoids pulling in `walkdir` crate).
fn walkdir(
    dir: &std::path::Path,
) -> anyhow::Result<impl Iterator<Item = anyhow::Result<std::fs::DirEntry>>> {
    fn collect(
        dir: &std::path::Path,
        out: &mut Vec<anyhow::Result<std::fs::DirEntry>>,
    ) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, out);
                } else {
                    out.push(Ok(entry));
                }
            }
        }
    }

    let mut entries = Vec::new();
    collect(dir, &mut entries);
    Ok(entries.into_iter())
}

/// Check whether a file has the executable bit set (Unix).
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    // On Windows we don't have a reliable cross-platform check here.
    false
}

/// Return the current UTC time as an ISO 8601 string.
///
/// Uses the system clock via `std::time::SystemTime` to avoid pulling in a
/// date/time crate.
fn current_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Format as a basic ISO 8601 UTC string: YYYY-MM-DDTHH:MM:SSZ
    let s = secs;
    let sec = s % 60;
    let min = (s / 60) % 60;
    let hour = (s / 3600) % 24;
    let days_since_epoch = s / 86400;

    // Gregorian calendar computation from day count.
    let (year, month, day) = days_to_ymd(days_since_epoch);

    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z"
    )
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_looks_sane() {
        let ts = current_timestamp();
        // Should be 20 characters: 2024-01-15T12:34:56Z
        assert_eq!(ts.len(), 20, "unexpected timestamp length: {ts}");
        assert!(ts.ends_with('Z'), "timestamp must end with Z: {ts}");
        assert!(ts.contains('T'), "timestamp must contain T: {ts}");
    }

    #[test]
    fn days_to_ymd_epoch() {
        let (y, m, d) = days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        // 2024-03-15 = 19797 days since epoch
        // (calculated: (2024-1970)*365 + leap days + day-of-year offset)
        let (y, m, d) = days_to_ymd(19797);
        assert_eq!(y, 2024);
        assert_eq!(m, 3);
        assert_eq!(d, 15);
    }
}

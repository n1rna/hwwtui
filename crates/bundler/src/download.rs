//! GitHub Releases download logic.
//!
//! Downloads asset tarballs, streams progress via a `watch` channel, and
//! extracts them with an async tar + gzip pipeline.

use std::path::Path;

use anyhow::Context;
use emulators::WalletType;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::{BundleStatus, RemoteBundle};

// ── GitHub API types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

// ── Downloader ────────────────────────────────────────────────────────────────

/// Downloads bundles from GitHub Releases.
pub struct GithubDownloader {
    client: Client,
    /// `"owner/repo"` e.g. `"user/hwwtui"`.
    repo: String,
}

impl GithubDownloader {
    /// Create a new downloader for the given `owner/repo`.
    pub fn new(repo: &str) -> Self {
        let client = Client::builder()
            .user_agent("hwwtui/0.1.0")
            .build()
            .expect("failed to build reqwest client");
        Self {
            client,
            repo: repo.to_owned(),
        }
    }

    /// Fetch the latest GitHub release and return all recognised bundle assets.
    ///
    /// Assets are recognised by the naming convention
    /// `hwwtui-{wallet}-{platform}.tar.gz`.  Unknown assets are silently
    /// skipped.
    ///
    /// Returns an error if GitHub responds with a non-200 status (including
    /// 403 rate-limit responses).
    pub async fn fetch_available(&self) -> anyhow::Result<Vec<RemoteBundle>> {
        let url = format!("https://api.github.com/repos/{}/releases/latest", self.repo);

        tracing::debug!(url = %url, "fetching latest release");

        let response = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("failed to send GitHub API request")?;

        let status = response.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub API rate limit exceeded (HTTP 403): {}",
                body.lines().next().unwrap_or("(no message)")
            );
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub API returned HTTP {}: {}",
                status,
                body.lines().next().unwrap_or("(no message)")
            );
        }

        let release: GithubRelease = response
            .json()
            .await
            .context("failed to parse GitHub release JSON")?;

        tracing::info!(tag = %release.tag_name, "found release");

        let bundles = release
            .assets
            .into_iter()
            .filter_map(|asset| {
                let (wallet_type, platform) = parse_asset_name(&asset.name)?;
                Some(RemoteBundle {
                    wallet_type,
                    version: release.tag_name.clone(),
                    platform,
                    download_url: asset.browser_download_url,
                    size_bytes: asset.size,
                    asset_name: asset.name,
                })
            })
            .collect();

        Ok(bundles)
    }

    /// Stream-download an asset URL to `dest`, sending progress updates.
    ///
    /// The file is written to `dest.with_extension(".tmp")` and renamed to
    /// `dest` only after the download completes successfully.
    ///
    /// `progress_tx` receives [`BundleStatus::Downloading`] updates while the
    /// transfer is in progress.
    pub async fn download_asset(
        &self,
        url: &str,
        dest: &Path,
        progress_tx: &tokio::sync::watch::Sender<BundleStatus>,
    ) -> anyhow::Result<()> {
        tracing::info!(url = %url, dest = %dest.display(), "starting download");

        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("failed to initiate download")?;

        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("download request returned HTTP {status}");
        }

        let total_bytes: Option<u64> = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok());

        let tmp = dest.with_extension("tar.gz.tmp");
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .with_context(|| format!("failed to create tmp file: {}", tmp.display()))?;

        let mut downloaded: u64 = 0;
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("error reading download stream")?;
            file.write_all(&chunk)
                .await
                .context("failed to write chunk to disk")?;
            downloaded += chunk.len() as u64;

            let pct = match total_bytes {
                Some(total) if total > 0 => ((downloaded * 100) / total).min(100) as u8,
                _ => 0,
            };

            // Ignore send errors — the receiver may have been dropped.
            let _ = progress_tx.send(BundleStatus::Downloading { progress_pct: pct });
        }

        file.flush()
            .await
            .context("failed to flush download file")?;
        drop(file);

        std::fs::rename(&tmp, dest)
            .with_context(|| format!("failed to rename {} -> {}", tmp.display(), dest.display()))?;

        tracing::info!(
            dest = %dest.display(),
            bytes = downloaded,
            "download complete"
        );
        Ok(())
    }
}

// ── Tarball extraction ────────────────────────────────────────────────────────

/// Extract a `.tar.gz` archive to `dest_dir` using async I/O.
///
/// The extraction runs on the async runtime.  CPU-bound decompression work is
/// offloaded via `spawn_blocking` to avoid blocking the executor.
pub async fn extract_tarball(archive_path: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let archive_path = archive_path.to_owned();
    let dest_dir = dest_dir.to_owned();

    tracing::debug!(
        archive = %archive_path.display(),
        dest = %dest_dir.display(),
        "extracting tarball"
    );

    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dest_dir)
            .with_context(|| format!("failed to create dest dir: {}", dest_dir.display()))?;

        let file = std::fs::File::open(&archive_path)
            .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;

        let gz = GzDecoder::new(file);
        let mut archive = Archive::new(gz);

        // Unpack but strip the top-level directory if there is one, so all
        // contents land directly in dest_dir.
        for entry in archive
            .entries()
            .context("failed to read archive entries")?
        {
            let mut entry = entry.context("failed to read archive entry")?;
            let entry_path = entry
                .path()
                .context("archive entry has invalid path")?
                .into_owned();

            // Strip the first component (e.g. "hwwtui-trezor-linux-x86_64/")
            // so we don't nest an extra directory.
            let stripped = if entry_path.components().count() > 1 {
                entry_path
                    .components()
                    .skip(1)
                    .collect::<std::path::PathBuf>()
            } else {
                // Top-level directory itself — skip it.
                continue;
            };

            if stripped.as_os_str().is_empty() {
                continue;
            }

            let out_path = dest_dir.join(&stripped);

            if entry.header().entry_type().is_dir() {
                std::fs::create_dir_all(&out_path).with_context(|| {
                    format!("failed to create directory: {}", out_path.display())
                })?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create parent dir: {}", parent.display())
                    })?;
                }
                entry.unpack(&out_path).with_context(|| {
                    format!("failed to unpack entry to: {}", out_path.display())
                })?;
            }
        }

        tracing::info!(dest = %dest_dir.display(), "extraction complete");
        Ok(())
    })
    .await
    .context("extraction task panicked")?
}

// ── Asset name parsing ────────────────────────────────────────────────────────

/// Parse an asset filename into `(WalletType, platform_string)`.
///
/// Expected format: `hwwtui-{wallet}-{platform}.tar.gz`
///
/// ```text
/// hwwtui-trezor-linux-x86_64.tar.gz  -> (Trezor, "linux-x86_64")
/// hwwtui-bitbox02-linux-x86_64.tar.gz -> (BitBox02, "linux-x86_64")
/// ```
///
/// Returns `None` for names that do not match the convention or contain an
/// unrecognised wallet identifier.
pub fn parse_asset_name(name: &str) -> Option<(WalletType, String)> {
    // Must end with ".tar.gz"
    let stem = name.strip_suffix(".tar.gz")?;

    // Must start with "hwwtui-"
    let rest = stem.strip_prefix("hwwtui-")?;

    // The wallet identifier may itself contain a hyphen (e.g. "bitbox02" does
    // not, but future names might).  We try each known wallet name as a
    // prefix.
    for (prefix, wallet_type) in WALLET_PREFIXES {
        if let Some(platform) = rest.strip_prefix(prefix) {
            // After the wallet name there must be a '-' followed by the
            // platform string.
            if let Some(platform) = platform.strip_prefix('-') {
                if !platform.is_empty() {
                    return Some((*wallet_type, platform.to_owned()));
                }
            }
        }
    }

    None
}

/// Ordered list of `(asset_name_segment, WalletType)` pairs.
///
/// Longer / more specific names must appear before shorter ones to avoid
/// incorrect prefix matches (e.g. `"bitbox02"` before `"bitbox"`).
const WALLET_PREFIXES: &[(&str, WalletType)] = &[
    ("bitbox02", WalletType::BitBox02),
    ("coldcard", WalletType::Coldcard),
    ("specter", WalletType::Specter),
    ("trezor", WalletType::Trezor),
    ("ledger", WalletType::Ledger),
    ("jade", WalletType::Jade),
];

/// Return the platform string for the current compile target.
///
/// ```text
/// Linux x86_64   -> "linux-x86_64"
/// macOS aarch64  -> "macos-aarch64"
/// ```
pub fn current_platform() -> String {
    let os = std::env::consts::OS; // "linux", "macos", "windows", …
    let arch = std::env::consts::ARCH; // "x86_64", "aarch64", …
    format!("{os}-{arch}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_assets() {
        let cases = [
            (
                "hwwtui-trezor-linux-x86_64.tar.gz",
                WalletType::Trezor,
                "linux-x86_64",
            ),
            (
                "hwwtui-bitbox02-linux-x86_64.tar.gz",
                WalletType::BitBox02,
                "linux-x86_64",
            ),
            (
                "hwwtui-coldcard-linux-aarch64.tar.gz",
                WalletType::Coldcard,
                "linux-aarch64",
            ),
            (
                "hwwtui-specter-macos-x86_64.tar.gz",
                WalletType::Specter,
                "macos-x86_64",
            ),
            (
                "hwwtui-ledger-macos-aarch64.tar.gz",
                WalletType::Ledger,
                "macos-aarch64",
            ),
            (
                "hwwtui-jade-linux-x86_64.tar.gz",
                WalletType::Jade,
                "linux-x86_64",
            ),
        ];

        for (name, expected_wallet, expected_platform) in &cases {
            let result = parse_asset_name(name);
            assert!(result.is_some(), "failed to parse asset name: {name}");
            let (wallet, platform) = result.unwrap();
            assert_eq!(wallet, *expected_wallet, "wallet mismatch for {name}");
            assert_eq!(
                platform, *expected_platform,
                "platform mismatch for {name}"
            );
        }
    }

    #[test]
    fn parse_unknown_assets_return_none() {
        let bad = [
            "trezor-linux-x86_64.tar.gz",         // missing hwwtui- prefix
            "hwwtui-trezor-linux-x86_64.zip",     // wrong extension
            "hwwtui-unknown-linux-x86_64.tar.gz", // unrecognised wallet
            "hwwtui-trezor.tar.gz",               // missing platform
        ];
        for name in &bad {
            assert!(
                parse_asset_name(name).is_none(),
                "expected None for: {name}"
            );
        }
    }

    #[test]
    fn current_platform_non_empty() {
        let p = current_platform();
        assert!(p.contains('-'), "expected 'os-arch' format, got: {p}");
    }
}

//! Application state and action dispatch.
//!
//! `App` is the single source of truth for all mutable TUI state. It owns
//! the list of `DevicePane`s (one per wallet type), the selected tab index,
//! and the bridge message channels.

use std::collections::VecDeque;
use std::sync::Arc;

use bridge::{trezor::TrezorBridge, Bridge, InterceptedMessage};
use bundler::{BundleManager, BundleStatus, RemoteBundle};
use emulators::{trezor::TrezorEmulator, Emulator, EmulatorStatus, WalletType};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::config::{Config, DeviceKind};

// ── Actions ───────────────────────────────────────────────────────────────────

/// User-triggered actions produced by the event loop and consumed by `process_actions`.
#[derive(Debug, Clone)]
pub enum Action {
    NextTab,
    PrevTab,
    StartSelected,
    StopSelected,
    ResetSelected,
    DownloadSelected,
    RemoveSelected,
    RefreshBundleStatus,
}

// ── Per-device state ──────────────────────────────────────────────────────────

/// Maximum number of log entries kept per device.
const MAX_LOG_ENTRIES: usize = 200;

/// All state for a single device panel.
pub struct DevicePane {
    pub kind: DeviceKind,
    pub label: String,
    /// The emulator process manager (None until started from bundle or explicit path).
    pub emulator: Option<Box<dyn Emulator>>,
    /// UHID bridge (None if not started or not yet implemented).
    pub bridge: Option<Box<dyn Bridge>>,
    /// Channel for intercepted bridge messages.
    pub bridge_rx: Option<mpsc::UnboundedReceiver<InterceptedMessage>>,
    /// Method-call log (decoded, human-readable).
    pub method_log: VecDeque<(String, String)>, // (direction prefix, message)
    /// Raw hex dump log.
    pub raw_log: VecDeque<(String, String)>, // (direction prefix, hex)
    /// Human-readable transport description.
    pub transport_label: String,
    /// Whether this device is fully implemented in Phase 1.
    pub implemented: bool,
    /// Current bundle installation status.
    pub bundle_status: BundleStatus,
    /// Watch receiver for download progress updates.
    pub download_progress_rx: Option<watch::Receiver<BundleStatus>>,
}

impl DevicePane {
    fn new(
        kind: DeviceKind,
        emulator: Option<Box<dyn Emulator>>,
        implemented: bool,
        bundle_status: BundleStatus,
    ) -> Self {
        let label = kind.to_string();
        let transport_label = emulator
            .as_ref()
            .map(|e| e.transport().to_string())
            .unwrap_or_else(|| "—".to_string());
        Self {
            kind,
            label,
            emulator,
            bridge: None,
            bridge_rx: None,
            method_log: VecDeque::with_capacity(MAX_LOG_ENTRIES),
            raw_log: VecDeque::with_capacity(MAX_LOG_ENTRIES),
            transport_label,
            implemented,
            bundle_status,
            download_progress_rx: None,
        }
    }

    pub fn status_str(&self) -> String {
        match &self.emulator {
            Some(e) => e.status().to_string(),
            None => "N/A".to_string(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.emulator
            .as_ref()
            .map(|e| e.status() == EmulatorStatus::Running)
            .unwrap_or(false)
    }

    /// Push a decoded message to the method log (capped).
    pub fn push_method(&mut self, direction: &str, text: String) {
        if self.method_log.len() >= MAX_LOG_ENTRIES {
            self.method_log.pop_front();
        }
        self.method_log.push_back((direction.to_string(), text));
    }

    /// Push a raw hex line to the raw log (capped).
    pub fn push_raw(&mut self, direction: &str, hex: String) {
        if self.raw_log.len() >= MAX_LOG_ENTRIES {
            self.raw_log.pop_front();
        }
        // Truncate long hex strings for display.
        let truncated = if hex.len() > 80 {
            format!("{}…", &hex[..80])
        } else {
            hex
        };
        self.raw_log.push_back((direction.to_string(), truncated));
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

/// Top-level application state.
pub struct App {
    /// One pane per wallet device type.
    pub panes: Vec<DevicePane>,
    /// Index of the currently selected tab.
    pub selected_tab: usize,
    /// Pending actions to process at the end of each event-loop tick.
    pending_actions: Vec<Action>,
    /// Set to true when the user requests exit.
    pub quit: bool,
    /// Bundle manager shared with background download tasks.
    pub bundle_manager: Arc<BundleManager>,
}

impl App {
    pub fn new(cfg: Config) -> anyhow::Result<Self> {
        let bundle_manager = Arc::new(BundleManager::new(&cfg.github_repo)?);

        let panes = cfg
            .devices
            .into_iter()
            .map(|dev_cfg| {
                let wallet_type = device_kind_to_wallet_type(dev_cfg.kind);
                let bundle_status = bundle_manager.status(wallet_type);

                // Build a Trezor emulator from the explicit config path if provided.
                // If not, we defer to `start_selected` which will use the bundle path.
                let emulator: Option<Box<dyn Emulator>> = match dev_cfg.kind {
                    DeviceKind::Trezor => {
                        if let Some(tc) = dev_cfg.trezor {
                            if let Some(firmware_path) = tc.firmware_path {
                                Some(Box::new(TrezorEmulator::new(
                                    firmware_path,
                                    tc.profile_dir,
                                    tc.port,
                                )))
                            } else {
                                // No explicit firmware path; will be resolved from bundle at start time.
                                None
                            }
                        } else {
                            None
                        }
                    }
                    // Phase 2+ devices: no emulator yet.
                    _ => None,
                };

                // All devices are shown as implemented — users can download any bundle.
                DevicePane::new(dev_cfg.kind, emulator, true, bundle_status)
            })
            .collect();

        Ok(Self {
            panes,
            selected_tab: 0,
            pending_actions: Vec::new(),
            quit: false,
            bundle_manager,
        })
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn selected_pane(&self) -> &DevicePane {
        &self.panes[self.selected_tab]
    }

    pub fn selected_pane_mut(&mut self) -> &mut DevicePane {
        &mut self.panes[self.selected_tab]
    }

    // ── Action dispatch ───────────────────────────────────────────────────────

    pub fn dispatch(&mut self, action: Action) {
        self.pending_actions.push(action);
    }

    pub async fn process_actions(&mut self) {
        let actions: Vec<Action> = self.pending_actions.drain(..).collect();
        for action in actions {
            match action {
                Action::NextTab => {
                    self.selected_tab = (self.selected_tab + 1) % self.panes.len();
                }
                Action::PrevTab => {
                    if self.selected_tab == 0 {
                        self.selected_tab = self.panes.len() - 1;
                    } else {
                        self.selected_tab -= 1;
                    }
                }
                Action::StartSelected => {
                    self.start_selected().await;
                }
                Action::StopSelected => {
                    self.stop_selected().await;
                }
                Action::ResetSelected => {
                    self.stop_selected().await;
                    self.start_selected().await;
                }
                Action::DownloadSelected => {
                    self.download_selected().await;
                }
                Action::RemoveSelected => {
                    self.remove_selected();
                }
                Action::RefreshBundleStatus => {
                    self.refresh_bundle_status();
                }
            }
        }
    }

    // ── Emulator / bridge lifecycle ───────────────────────────────────────────

    async fn start_selected(&mut self) {
        let idx = self.selected_tab;
        let pane = &mut self.panes[idx];

        // If emulator is already set, use it directly.
        // If not, try to build one from the installed bundle.
        if pane.emulator.is_none() {
            match pane.kind {
                DeviceKind::Trezor => {
                    let wallet_type = device_kind_to_wallet_type(pane.kind);
                    let firmware_dir = self.bundle_manager.firmware_dir(wallet_type);
                    match firmware_dir {
                        Some(fw_dir) => {
                            let profile_dir = PathBuf::from("/tmp/hwwtui-trezor");
                            let port = 21324u16;
                            let emu = TrezorEmulator::new(fw_dir, profile_dir, port);
                            pane.emulator = Some(Box::new(emu));
                            pane.transport_label = format!("UDP :{port}");
                        }
                        None => {
                            pane.push_method(
                                "!",
                                "No bundle installed. Press [d] to download the firmware bundle."
                                    .to_string(),
                            );
                            return;
                        }
                    }
                }
                _ => {
                    // Other devices: check bundle but no emulator implemented yet.
                    let wallet_type = device_kind_to_wallet_type(pane.kind);
                    if matches!(self.bundle_manager.status(wallet_type), BundleStatus::NotInstalled) {
                        pane.push_method(
                            "!",
                            "No bundle installed. Press [d] to download the firmware bundle."
                                .to_string(),
                        );
                    } else {
                        pane.push_method(
                            "!",
                            format!("{} emulator not yet implemented.", pane.label),
                        );
                    }
                    return;
                }
            }
        }

        // Start the emulator process.
        if let Some(emu) = &mut self.panes[idx].emulator {
            if let Err(e) = emu.start().await {
                error!("Failed to start emulator: {e:#}");
                self.panes[idx].push_method("!", format!("Start failed: {e:#}"));
                return;
            }
        }

        // Start the UHID bridge.
        let bridge_result = match self.panes[idx].kind {
            DeviceKind::Trezor => {
                let port = self.panes[idx]
                    .emulator
                    .as_ref()
                    .and_then(|e| {
                        if let emulators::TransportConfig::Udp { port, .. } = e.transport() {
                            Some(port)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(21324);

                let mut bridge = TrezorBridge::new("127.0.0.1", port);
                match bridge.start().await {
                    Ok(rx) => {
                        self.panes[idx].transport_label = format!("UDP :{port}");
                        self.panes[idx].push_method("→", "Bridge started".to_string());
                        Some((Box::new(bridge) as Box<dyn Bridge>, rx))
                    }
                    Err(e) => {
                        error!("Failed to start bridge: {e:#}");
                        self.panes[idx].push_method("!", format!("Bridge failed: {e:#}"));
                        None
                    }
                }
            }
            _ => None,
        };

        if let Some((bridge, rx)) = bridge_result {
            let label = self.panes[idx].label.clone();
            self.panes[idx].bridge = Some(bridge);
            self.panes[idx].bridge_rx = Some(rx);
            info!("Device {} started", label);
        }
    }

    async fn stop_selected(&mut self) {
        let idx = self.selected_tab;
        let pane = &mut self.panes[idx];

        if let Some(bridge) = &mut pane.bridge {
            if let Err(e) = bridge.stop().await {
                error!("Bridge stop error: {e:#}");
            }
        }
        pane.bridge = None;
        pane.bridge_rx = None;

        if let Some(emu) = &mut pane.emulator {
            if let Err(e) = emu.stop().await {
                error!("Emulator stop error: {e:#}");
            }
        }

        pane.push_method("■", "Stopped".to_string());
        info!("Device {} stopped", pane.label);
    }

    /// Stop all running devices (called on quit).
    pub async fn stop_all(&mut self) {
        for idx in 0..self.panes.len() {
            let pane = &mut self.panes[idx];
            if let Some(bridge) = &mut pane.bridge {
                bridge.stop().await.ok();
            }
            pane.bridge = None;
            pane.bridge_rx = None;
            if let Some(emu) = &mut pane.emulator {
                emu.stop().await.ok();
            }
        }
        self.quit = true;
    }

    // ── Bundle management ─────────────────────────────────────────────────────

    /// Initiate a background download for the currently selected device.
    async fn download_selected(&mut self) {
        let idx = self.selected_tab;
        let pane = &mut self.panes[idx];
        let wallet_type = device_kind_to_wallet_type(pane.kind);

        // Show "fetching" state while we query GitHub.
        pane.bundle_status = BundleStatus::Downloading { progress_pct: 0 };
        pane.push_method("→", "Fetching bundle list from GitHub…".to_string());

        let manager = Arc::clone(&self.bundle_manager);

        // Create the watch channel for progress updates.
        let (progress_tx, progress_rx) = watch::channel(BundleStatus::Downloading { progress_pct: 0 });
        pane.download_progress_rx = Some(progress_rx);

        tokio::spawn(async move {
            // Fetch available remote bundles.
            let remotes: Vec<RemoteBundle> = match manager.list_remote().await {
                Ok(list) => list,
                Err(e) => {
                    let msg = format!("Failed to fetch bundle list: {e:#}");
                    warn!("{msg}");
                    let _ = progress_tx.send(BundleStatus::Failed { error: msg });
                    return;
                }
            };

            // Find the bundle matching this wallet type.
            let bundle = remotes.into_iter().find(|b| b.wallet_type == wallet_type);
            let bundle = match bundle {
                Some(b) => b,
                None => {
                    let msg = format!("No bundle found for {wallet_type} on this platform");
                    warn!("{msg}");
                    let _ = progress_tx.send(BundleStatus::Failed { error: msg });
                    return;
                }
            };

            info!(
                wallet = %wallet_type,
                version = %bundle.version,
                "Starting bundle download"
            );

            if let Err(e) = manager.download(&bundle, progress_tx).await {
                error!("Bundle download failed for {wallet_type}: {e:#}");
            }
        });
    }

    /// Remove the bundle for the currently selected device.
    fn remove_selected(&mut self) {
        let idx = self.selected_tab;
        let pane = &mut self.panes[idx];
        let wallet_type = device_kind_to_wallet_type(pane.kind);

        match self.bundle_manager.remove(wallet_type) {
            Ok(()) => {
                pane.bundle_status = BundleStatus::NotInstalled;
                // Clear any emulator built from the bundle so it won't be
                // started again with a path that no longer exists.
                pane.emulator = None;
                pane.transport_label = "—".to_string();
                pane.push_method("■", "Bundle removed".to_string());
                info!("Bundle removed for {}", pane.label);
            }
            Err(e) => {
                error!("Failed to remove bundle: {e:#}");
                pane.push_method("!", format!("Remove failed: {e:#}"));
            }
        }
    }

    /// Re-query the bundle manager for each pane's current status.
    fn refresh_bundle_status(&mut self) {
        for pane in &mut self.panes {
            let wallet_type = device_kind_to_wallet_type(pane.kind);
            pane.bundle_status = self.bundle_manager.status(wallet_type);
        }
    }

    /// Poll watch channels for download progress updates. Called every tick.
    pub fn poll_download_progress(&mut self) {
        for pane in &mut self.panes {
            // Borrow the receiver separately to satisfy the borrow checker.
            let new_status: Option<BundleStatus> = if let Some(rx) = &mut pane.download_progress_rx {
                // `has_changed` is true if a new value was sent since last borrow.
                if rx.has_changed().unwrap_or(false) {
                    Some(rx.borrow_and_update().clone())
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(status) = new_status {
                let done = matches!(
                    &status,
                    BundleStatus::Installed { .. } | BundleStatus::Failed { .. }
                );
                pane.bundle_status = status;
                if done {
                    // Drop the receiver — download task has finished.
                    pane.download_progress_rx = None;
                    if matches!(pane.bundle_status, BundleStatus::Installed { .. }) {
                        pane.push_method("→", "Bundle installed successfully".to_string());
                    }
                }
            }
        }
    }

    // ── Bridge message drain ──────────────────────────────────────────────────

    /// Drain all pending bridge messages into the pane log buffers.
    /// Called every tick from the event loop (non-blocking).
    pub fn poll_bridge_messages(&mut self) {
        for pane in &mut self.panes {
            // Collect messages first into a temporary Vec so that we hold no
            // borrow on `pane.bridge_rx` when we later call push_* methods.
            let messages: Vec<InterceptedMessage> = {
                let rx = match &mut pane.bridge_rx {
                    Some(r) => r,
                    None => continue,
                };
                let mut batch = Vec::new();
                loop {
                    match rx.try_recv() {
                        Ok(msg) => batch.push(msg),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                    }
                }
                batch
            };

            for msg in messages {
                let dir = msg.direction.to_string();
                pane.push_raw(&dir, msg.raw_hex.clone());
                if let Some(decoded) = &msg.decoded {
                    pane.push_method(&dir, decoded.clone());
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a `DeviceKind` to the corresponding `WalletType` used by the bundler.
pub fn device_kind_to_wallet_type(kind: DeviceKind) -> WalletType {
    match kind {
        DeviceKind::Trezor => WalletType::Trezor,
        DeviceKind::BitBox02 => WalletType::BitBox02,
        DeviceKind::Coldcard => WalletType::Coldcard,
        DeviceKind::Specter => WalletType::Specter,
        DeviceKind::Ledger => WalletType::Ledger,
        DeviceKind::Jade => WalletType::Jade,
    }
}

/// Format a byte count as a human-readable string: `"40.1 MB"`, `"1.2 GB"`, etc.
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * KB;
    const GB: u64 = 1_024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// Bring PathBuf into scope for start_selected.
use std::path::PathBuf;

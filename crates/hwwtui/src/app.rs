//! Application state and action dispatch.
//!
//! `App` is the single source of truth for all mutable TUI state. It owns
//! the list of `DevicePane`s (one per wallet type), the selected tab index,
//! and the bridge message channels.

use std::collections::VecDeque;
use std::sync::Arc;

use bridge::{trezor::TrezorBridge, Bridge, InterceptedMessage};
use bundler::{BundleManager, BundleStatus, RemoteBundle};
use emulators::{
    generic::GenericEmulator, trezor::TrezorEmulator, Emulator, EmulatorStatus, TransportConfig,
    WalletType,
};
use protocol::trezor_debug::{
    parse_layout_tokens, DebugButton, ParsedLayout, SwipeDirection, TrezorDebugLink,
    TrezorWireClient,
};
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
    #[allow(dead_code)]
    RefreshBundleStatus,
    /// Send Initialize to the emulator (triggers Features response).
    InitializeDevice,
    /// Press YES on the emulator via the debug link.
    ConfirmSelected,
    /// Press NO on the emulator via the debug link.
    CancelSelected,
    SwipeUp,
    SwipeDown,
    SwipeLeft,
    SwipeRight,
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
    /// Lines captured from the emulator process stdout/stderr.
    pub firmware_log: VecDeque<String>,
    /// Human-readable transport description.
    pub transport_label: String,
    /// Whether this device is fully implemented.
    #[allow(dead_code)]
    pub implemented: bool,
    /// Current bundle installation status.
    pub bundle_status: BundleStatus,
    /// Watch receiver for download progress updates.
    pub download_progress_rx: Option<watch::Receiver<BundleStatus>>,
    /// Debug link client for the Trezor emulator (UDP port 21325).
    pub debug_link: Option<TrezorDebugLink>,
    pub wire_client: Option<TrezorWireClient>,
    /// Screen title extracted from the last debug-link poll.
    pub screen_title: String,
    /// Screen text content lines from the last debug-link poll.
    pub screen_content: Vec<String>,
    /// Button labels shown on the current emulator screen.
    pub screen_buttons: Vec<String>,
    /// Tick counter used to throttle debug-link polling.
    pub screen_poll_ticks: u32,
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
            firmware_log: VecDeque::with_capacity(MAX_LOG_ENTRIES),
            transport_label,
            implemented,
            bundle_status,
            download_progress_rx: None,
            debug_link: None,
            wire_client: None,
            screen_title: String::new(),
            screen_content: Vec::new(),
            screen_buttons: Vec::new(),
            screen_poll_ticks: 0,
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

    /// Push a line from the emulator process stdout/stderr (capped).
    pub fn push_firmware_log(&mut self, line: String) {
        if self.firmware_log.len() >= MAX_LOG_ENTRIES {
            self.firmware_log.pop_front();
        }
        // Strip ANSI escape codes so they don't corrupt the TUI.
        let clean = strip_ansi(&line);
        self.firmware_log.push_back(clean);
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

    #[allow(dead_code)]
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
                Action::InitializeDevice => {
                    self.wire_initialize().await;
                }
                Action::ConfirmSelected => {
                    self.debug_press(DebugButton::Yes).await;
                }
                Action::CancelSelected => {
                    self.debug_press(DebugButton::No).await;
                }
                Action::SwipeUp => {
                    self.debug_swipe(SwipeDirection::Up).await;
                }
                Action::SwipeDown => {
                    self.debug_swipe(SwipeDirection::Down).await;
                }
                Action::SwipeLeft => {
                    self.debug_swipe(SwipeDirection::Left).await;
                }
                Action::SwipeRight => {
                    self.debug_swipe(SwipeDirection::Right).await;
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
                    let binary_path = self.bundle_manager.emulator_binary_path(wallet_type);
                    match binary_path {
                        Some(bin_path) => {
                            // The bundle dir is the parent of the binary.
                            // TrezorEmulator expects firmware_path to contain
                            // build/unix/trezor-emu-core, so we set it up so
                            // micropython_path() resolves correctly.
                            let bundle_dir =
                                bin_path.parent().unwrap_or(bin_path.as_ref()).to_path_buf();
                            let profile_dir = PathBuf::from("/tmp/hwwtui-trezor");
                            let port = 21324u16;
                            let emu = TrezorEmulator::new_with_binary(
                                bin_path,
                                bundle_dir,
                                profile_dir,
                                port,
                            );
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
                DeviceKind::BitBox02 => {
                    let wallet_type = device_kind_to_wallet_type(pane.kind);
                    match self.bundle_manager.emulator_binary_path(wallet_type) {
                        Some(bin_path) => {
                            let bundle_dir =
                                bin_path.parent().unwrap_or(bin_path.as_ref()).to_path_buf();
                            let emu = GenericEmulator::new(
                                WalletType::BitBox02,
                                bin_path,
                                bundle_dir,
                                PathBuf::from("/tmp/hwwtui-bitbox02"),
                                TransportConfig::Tcp {
                                    host: "127.0.0.1".into(),
                                    port: 15423,
                                },
                            );
                            pane.emulator = Some(Box::new(emu));
                            pane.transport_label = "TCP :15423".to_string();
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
                DeviceKind::Coldcard => {
                    let wallet_type = device_kind_to_wallet_type(pane.kind);
                    match self.bundle_manager.emulator_binary_path(wallet_type) {
                        Some(bin_path) => {
                            // The Coldcard simulator must be launched from unix/ with
                            // micropython as the binary and simulator.py as the script.
                            let bundle_dir =
                                bin_path.parent().unwrap_or(bin_path.as_ref()).to_path_buf();
                            let shared_dir = bundle_dir.join("shared");
                            let unix_dir = bundle_dir.join("unix");
                            let micropypath = shared_dir.display().to_string();
                            let emu = GenericEmulator::new(
                                WalletType::Coldcard,
                                bin_path,
                                unix_dir,
                                PathBuf::from("/tmp/hwwtui-coldcard"),
                                TransportConfig::UnixSocket {
                                    path: PathBuf::from("/tmp/ckcc-simulator.sock"),
                                },
                            )
                            .with_env("MICROPYPATH", &micropypath)
                            .with_arg("-i")
                            .with_arg("./unix/simulator.py");
                            pane.emulator = Some(Box::new(emu));
                            pane.transport_label = "Unix /tmp/ckcc-simulator.sock".to_string();
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
                DeviceKind::Specter => {
                    let wallet_type = device_kind_to_wallet_type(pane.kind);
                    match self.bundle_manager.emulator_binary_path(wallet_type) {
                        Some(bin_path) => {
                            let bundle_dir =
                                bin_path.parent().unwrap_or(bin_path.as_ref()).to_path_buf();
                            let src_dir = bundle_dir.join("src");
                            let micropypath = src_dir.display().to_string();
                            let emu = GenericEmulator::new(
                                WalletType::Specter,
                                bin_path,
                                bundle_dir,
                                PathBuf::from("/tmp/hwwtui-specter"),
                                TransportConfig::Tcp {
                                    host: "127.0.0.1".into(),
                                    port: 8789,
                                },
                            )
                            .with_env("MICROPYPATH", &micropypath);
                            pane.emulator = Some(Box::new(emu));
                            pane.transport_label = "TCP :8789".to_string();
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
                DeviceKind::Ledger => {
                    let wallet_type = device_kind_to_wallet_type(pane.kind);
                    match self.bundle_manager.emulator_binary_path(wallet_type) {
                        Some(bin_path) => {
                            let bundle_dir =
                                bin_path.parent().unwrap_or(bin_path.as_ref()).to_path_buf();
                            let emu = GenericEmulator::new(
                                WalletType::Ledger,
                                // The runner script lives in the bundle dir.
                                bundle_dir.join("speculos-runner.sh"),
                                bundle_dir,
                                PathBuf::from("/tmp/hwwtui-ledger"),
                                TransportConfig::Tcp {
                                    host: "127.0.0.1".into(),
                                    port: 9999,
                                },
                            )
                            .with_startup_timeout(std::time::Duration::from_secs(30));
                            pane.emulator = Some(Box::new(emu));
                            pane.transport_label = "TCP :9999".to_string();
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
                DeviceKind::Jade => {
                    let wallet_type = device_kind_to_wallet_type(pane.kind);
                    match self.bundle_manager.emulator_binary_path(wallet_type) {
                        Some(bin_path) => {
                            let bundle_dir =
                                bin_path.parent().unwrap_or(bin_path.as_ref()).to_path_buf();
                            let emu = GenericEmulator::new(
                                WalletType::Jade,
                                // The runner script loads the QEMU image via Docker.
                                bundle_dir.join("jade-runner.sh"),
                                bundle_dir,
                                PathBuf::from("/tmp/hwwtui-jade"),
                                TransportConfig::Tcp {
                                    host: "127.0.0.1".into(),
                                    port: 30121,
                                },
                            )
                            .with_startup_timeout(std::time::Duration::from_secs(30));
                            pane.emulator = Some(Box::new(emu));
                            pane.transport_label = "TCP :30121".to_string();
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

        // Connect the debug link for Trezor (port = main port + 1).
        if matches!(self.panes[idx].kind, DeviceKind::Trezor) {
            let debug_port = self.panes[idx]
                .emulator
                .as_ref()
                .and_then(|e| {
                    if let emulators::TransportConfig::Udp { port, .. } = e.transport() {
                        Some(port + 1)
                    } else {
                        None
                    }
                })
                .unwrap_or(21325);

            match TrezorDebugLink::connect(debug_port).await {
                Ok(dl) => {
                    self.panes[idx].debug_link = Some(dl);
                    self.panes[idx]
                        .push_method("→", format!("Debug link connected (UDP :{debug_port})"));
                }
                Err(e) => {
                    self.panes[idx].push_method("!", format!("Debug link failed: {e}"));
                }
            }

            // Also connect the wire client to the main port.
            let wire_port = debug_port - 1; // 21324
            match TrezorWireClient::connect(wire_port).await {
                Ok(wc) => {
                    self.panes[idx].wire_client = Some(wc);
                    self.panes[idx]
                        .push_method("→", format!("Wire client connected (UDP :{wire_port})"));
                }
                Err(e) => {
                    self.panes[idx].push_method("!", format!("Wire client failed: {e}"));
                }
            }
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
        pane.debug_link = None;
        pane.wire_client = None;
        pane.screen_title = String::new();
        pane.screen_content = Vec::new();
        pane.screen_buttons = Vec::new();
        pane.screen_poll_ticks = 0;

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
            pane.debug_link = None;
            pane.wire_client = None;
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
        let (progress_tx, progress_rx) =
            watch::channel(BundleStatus::Downloading { progress_pct: 0 });
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
            let new_status: Option<BundleStatus> = if let Some(rx) = &mut pane.download_progress_rx
            {
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

    // ── Firmware log drain ────────────────────────────────────────────────────

    /// Drain any stdout/stderr lines captured from running emulator processes.
    /// Called every tick from the event loop (non-blocking).
    pub fn poll_firmware_logs(&mut self) {
        for pane in &mut self.panes {
            if let Some(emu) = &mut pane.emulator {
                for line in emu.drain_output() {
                    pane.push_firmware_log(line);
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
    // ── Debug link ────────────────────────────────────────────────────────────

    /// Poll the debug-link screen for the currently selected pane.
    ///
    /// This is called every tick from the event loop but only issues a network
    /// request every 5 ticks (~500 ms at 100 ms/tick).  If the emulator is not
    /// running, or no debug link is connected, the call is a no-op.
    pub async fn poll_screen(&mut self) {
        let idx = self.selected_tab;
        let pane = &mut self.panes[idx];

        if !pane.is_running() {
            return;
        }

        // Throttle: only poll every 5 ticks.
        pane.screen_poll_ticks = pane.screen_poll_ticks.wrapping_add(1);
        if pane.screen_poll_ticks % 5 != 0 {
            return;
        }

        // We need to temporarily take the debug link out of the pane to avoid
        // a simultaneous mutable borrow.
        let Some(dl) = pane.debug_link.take() else {
            return;
        };

        match dl.get_layout().await {
            Ok(tokens) => {
                let parsed: ParsedLayout = parse_layout_tokens(&tokens);
                pane.screen_title = parsed.title;
                pane.screen_content = parsed.lines;
                pane.screen_buttons = parsed.buttons;
            }
            Err(e) => {
                tracing::debug!("Debug link poll failed: {e}");
            }
        }

        // Put the link back.
        pane.debug_link = Some(dl);
    }

    /// Send a button press through the debug link for the selected pane.
    async fn debug_press(&mut self, button: DebugButton) {
        let pane = &mut self.panes[self.selected_tab];
        let Some(dl) = pane.debug_link.take() else {
            return;
        };
        if let Err(e) = dl.press_button(button).await {
            pane.push_method("!", format!("Debug button failed: {e}"));
        }
        pane.debug_link = Some(dl);
    }

    /// Send a swipe gesture through the debug link for the selected pane.
    async fn debug_swipe(&mut self, direction: SwipeDirection) {
        let pane = &mut self.panes[self.selected_tab];
        let Some(dl) = pane.debug_link.take() else {
            return;
        };
        if let Err(e) = dl.swipe(direction).await {
            pane.push_method("!", format!("Debug swipe failed: {e}"));
        }
        pane.debug_link = Some(dl);
    }

    /// Send Initialize to the emulator's main wire port.
    async fn wire_initialize(&mut self) {
        let pane = &mut self.panes[self.selected_tab];
        let Some(wc) = pane.wire_client.take() else {
            pane.push_method("!", "Wire client not connected".to_string());
            return;
        };
        match wc.initialize().await {
            Ok(data) => {
                pane.push_method("→", format!("Initialize → Features ({} bytes)", data.len()));
            }
            Err(e) => {
                pane.push_method("!", format!("Initialize failed: {e}"));
            }
        }
        pane.wire_client = Some(wc);
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

/// Strip ANSI escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Consume ESC [ ... m  (CSI sequence)
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

// Bring PathBuf into scope for start_selected.
use std::path::PathBuf;

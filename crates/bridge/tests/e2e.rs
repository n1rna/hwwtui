//! End-to-end integration tests.
//!
//! These tests start real emulators, exercise protocol commands, and verify
//! that the UHID bridge makes them discoverable via `hidapi`.
//!
//! # Prerequisites
//!
//! - Emulator bundles must be installed (`just bundle-install <wallet>`)
//! - `/dev/uhid` must be writable (for bridge tests)
//!
//! # Running
//!
//! ```bash
//! # Run all E2E tests (needs bundles + /dev/uhid):
//! cargo test -p bridge --test e2e -- --ignored --test-threads=1
//!
//! # Just Trezor (no UHID needed):
//! cargo test -p bridge --test e2e trezor -- --ignored --test-threads=1
//! ```
//!
//! Note: `--test-threads=1` is required because Trezor tests share the
//! emulator UDP port and cannot run concurrently.

use std::path::Path;
use std::time::Duration;

use bundler::BundleManager;
use emulators::{Emulator, EmulatorStatus, TransportConfig, WalletType};
use protocol::trezor_debug::{
    extract_features_label, extract_xpub, TrezorDebugLink, TrezorWireClient,
};

// ── Helpers ─────────────────────────────────────────────────────────────────

const GITHUB_REPO: &str = "n1rna/hwwtui";
const TREZOR_PORT: u16 = 21324;

/// Standard BIP39 test mnemonic (12 words).
const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

fn bundle_manager() -> BundleManager {
    BundleManager::new(GITHUB_REPO).expect("BundleManager init failed")
}

fn bundle_installed(wallet: WalletType) -> bool {
    let mgr = bundle_manager();
    matches!(mgr.status(wallet), bundler::BundleStatus::Installed { .. })
}

fn uhid_available() -> bool {
    Path::new("/dev/uhid").exists()
        && std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uhid")
            .is_ok()
}

/// Start the Trezor emulator and wait until the wire protocol is responsive.
/// Wipes the profile directory to ensure a clean state.
async fn start_trezor_on(port: u16, profile_suffix: &str) -> Box<dyn Emulator> {
    let mgr = bundle_manager();
    let bin = mgr
        .emulator_binary_path(WalletType::Trezor)
        .expect("Trezor bundle not installed");
    let bundle_dir = bin.parent().unwrap().to_path_buf();
    let profile = std::path::PathBuf::from(format!("/tmp/hwwtui-test-trezor-{profile_suffix}"));

    // Wipe profile dir to ensure fresh state.
    if profile.exists() {
        std::fs::remove_dir_all(&profile).ok();
    }

    let mut emu: Box<dyn Emulator> = Box::new(emulators::trezor::TrezorEmulator::new_with_binary(
        bin, bundle_dir, profile, port,
    ));
    emu.start().await.expect("Trezor emulator failed to start");
    assert_eq!(emu.status(), EmulatorStatus::Running);

    // The UDP probe only checks port reachability, not wire protocol readiness.
    // Poll with actual Initialize messages until the emulator responds.
    wait_for_wire_ready(port).await;

    emu
}

/// Send Initialize until the emulator responds (up to 10s).
async fn wait_for_wire_ready(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(wc) = TrezorWireClient::connect(port).await {
            if wc.initialize().await.is_ok() {
                return;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("Trezor wire protocol not ready after 10s on port {port}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Stop emulator and wait for the port to be released.
async fn stop_trezor(mut emu: Box<dyn Emulator>) {
    emu.stop().await.expect("Stop failed");
    // Give the OS time to fully release the UDP port before the next test.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

// ── Trezor E2E ──────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Trezor bundle installed"]
async fn trezor_emulator_starts_and_responds_to_initialize() {
    if !bundle_installed(WalletType::Trezor) {
        eprintln!("SKIP: Trezor bundle not installed");
        return;
    }

    let emu = start_trezor_on(TREZOR_PORT, "init").await;

    let wc = TrezorWireClient::connect(TREZOR_PORT)
        .await
        .expect("Wire connect failed");
    let features = wc.initialize().await.expect("Initialize failed");
    assert!(
        !features.is_empty(),
        "Features response should not be empty"
    );

    stop_trezor(emu).await;
}

#[tokio::test]
#[ignore = "requires Trezor bundle installed"]
async fn trezor_load_seed_and_get_features() {
    if !bundle_installed(WalletType::Trezor) {
        eprintln!("SKIP: Trezor bundle not installed");
        return;
    }

    let emu = start_trezor_on(TREZOR_PORT, "seed").await;

    let wc = TrezorWireClient::connect(TREZOR_PORT)
        .await
        .expect("Wire connect failed");
    let dl = TrezorDebugLink::connect(TREZOR_PORT + 1)
        .await
        .expect("Debug link connect failed");

    wc.load_device(TEST_MNEMONIC, "e2e-test", Some(&dl))
        .await
        .expect("LoadDevice failed");

    // Need a fresh wire client after LoadDevice (emulator resets the session).
    let wc2 = TrezorWireClient::connect(TREZOR_PORT)
        .await
        .expect("Wire reconnect failed");
    let features = wc2.get_features().await.expect("GetFeatures failed");

    let label = extract_features_label(&features);
    assert_eq!(
        label.as_deref(),
        Some("e2e-test"),
        "Features label should match what we set in LoadDevice"
    );

    stop_trezor(emu).await;
}

#[tokio::test]
#[ignore = "requires Trezor bundle installed"]
async fn trezor_get_public_key_returns_valid_xpub() {
    if !bundle_installed(WalletType::Trezor) {
        eprintln!("SKIP: Trezor bundle not installed");
        return;
    }

    let emu = start_trezor_on(TREZOR_PORT, "xpub").await;

    let wc = TrezorWireClient::connect(TREZOR_PORT)
        .await
        .expect("Wire connect failed");
    let dl = TrezorDebugLink::connect(TREZOR_PORT + 1)
        .await
        .expect("Debug link connect failed");

    wc.load_device(TEST_MNEMONIC, "xpub-test", Some(&dl))
        .await
        .expect("LoadDevice failed");

    // Fresh client after seed load.
    let wc2 = TrezorWireClient::connect(TREZOR_PORT)
        .await
        .expect("Wire reconnect failed");
    let dl2 = TrezorDebugLink::connect(TREZOR_PORT + 1)
        .await
        .expect("Debug link reconnect failed");

    // Request the public key at m/84'/0'/0' (native segwit account 0).
    let path = &[0x80000000 | 84, 0x80000000, 0x80000000];
    let resp = wc2
        .get_public_key(path, Some(&dl2))
        .await
        .expect("GetPublicKey failed");

    assert!(!resp.is_empty(), "PublicKey response should not be empty");

    let xpub = extract_xpub(&resp);
    assert!(
        xpub.is_some(),
        "Should be able to extract xpub from response"
    );

    let xpub = xpub.unwrap();
    assert!(
        xpub.starts_with("xpub"),
        "xpub should start with 'xpub', got: {xpub}"
    );
    assert!(
        xpub.len() > 100 && xpub.len() < 120,
        "xpub length should be ~111 chars, got {} ({xpub})",
        xpub.len()
    );

    // Verify all characters are valid base58.
    let base58_chars = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    assert!(
        xpub.chars().all(|c| base58_chars.contains(c)),
        "xpub should only contain base58 characters: {xpub}"
    );

    stop_trezor(emu).await;
}

#[tokio::test]
#[ignore = "requires Trezor bundle installed"]
async fn trezor_multiple_derivation_paths() {
    if !bundle_installed(WalletType::Trezor) {
        eprintln!("SKIP: Trezor bundle not installed");
        return;
    }

    let emu = start_trezor_on(TREZOR_PORT, "multi").await;

    let wc = TrezorWireClient::connect(TREZOR_PORT)
        .await
        .expect("Wire connect failed");
    let dl = TrezorDebugLink::connect(TREZOR_PORT + 1)
        .await
        .expect("Debug link connect failed");

    wc.load_device(TEST_MNEMONIC, "multi-path-test", Some(&dl))
        .await
        .expect("LoadDevice failed");

    // Fresh clients after seed load.
    let wc2 = TrezorWireClient::connect(TREZOR_PORT)
        .await
        .expect("Wire reconnect failed");
    let dl2 = TrezorDebugLink::connect(TREZOR_PORT + 1)
        .await
        .expect("Debug link reconnect failed");

    let paths: &[(&str, &[u32])] = &[
        ("m/44'/0'/0'", &[0x80000000 | 44, 0x80000000, 0x80000000]),
        ("m/49'/0'/0'", &[0x80000000 | 49, 0x80000000, 0x80000000]),
        ("m/84'/0'/0'", &[0x80000000 | 84, 0x80000000, 0x80000000]),
    ];

    let mut xpubs = Vec::new();
    for (label, path) in paths {
        let resp = wc2
            .get_public_key(path, Some(&dl2))
            .await
            .unwrap_or_else(|e| panic!("GetPublicKey failed for {label}: {e}"));

        let xpub = extract_xpub(&resp).unwrap_or_else(|| panic!("No xpub in response for {label}"));
        assert!(
            xpub.starts_with("xpub"),
            "{label}: expected xpub prefix, got {xpub}"
        );
        xpubs.push((label.to_string(), xpub));
    }

    // All three xpubs should be different (different derivation paths).
    assert_ne!(
        xpubs[0].1, xpubs[1].1,
        "m/44' and m/49' xpubs should differ"
    );
    assert_ne!(
        xpubs[1].1, xpubs[2].1,
        "m/49' and m/84' xpubs should differ"
    );
    assert_ne!(
        xpubs[0].1, xpubs[2].1,
        "m/44' and m/84' xpubs should differ"
    );

    stop_trezor(emu).await;
}

#[tokio::test]
#[ignore = "requires Trezor bundle installed"]
async fn trezor_debug_link_reads_screen() {
    if !bundle_installed(WalletType::Trezor) {
        eprintln!("SKIP: Trezor bundle not installed");
        return;
    }

    let emu = start_trezor_on(TREZOR_PORT, "screen").await;

    let dl = TrezorDebugLink::connect(TREZOR_PORT + 1)
        .await
        .expect("Debug link connect failed");

    // Give the emulator a moment to render the homescreen.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let tokens = dl.get_layout().await.expect("get_layout failed");
    assert!(
        !tokens.is_empty(),
        "Screen layout should have at least one token"
    );

    // The concatenated tokens should form valid JSON.
    let json_str: String = tokens.concat();
    assert!(
        serde_json::from_str::<serde_json::Value>(&json_str).is_ok(),
        "Layout tokens should form valid JSON: {json_str}"
    );

    stop_trezor(emu).await;
}

#[tokio::test]
#[ignore = "requires Trezor bundle installed"]
async fn trezor_restart_preserves_no_state_leak() {
    if !bundle_installed(WalletType::Trezor) {
        eprintln!("SKIP: Trezor bundle not installed");
        return;
    }

    let profile = std::path::PathBuf::from("/tmp/hwwtui-test-trezor-restart");
    if profile.exists() {
        std::fs::remove_dir_all(&profile).ok();
    }

    let mgr = bundle_manager();
    let bin = mgr
        .emulator_binary_path(WalletType::Trezor)
        .expect("Trezor binary not found");
    let bundle_dir = bin.parent().unwrap().to_path_buf();

    // First run: load seed.
    {
        let mut emu = emulators::trezor::TrezorEmulator::new_with_binary(
            bin.clone(),
            bundle_dir.clone(),
            profile.clone(),
            TREZOR_PORT,
        );
        emu.start().await.unwrap();
        wait_for_wire_ready(TREZOR_PORT).await;

        let wc = TrezorWireClient::connect(TREZOR_PORT).await.unwrap();
        let dl = TrezorDebugLink::connect(TREZOR_PORT + 1).await.unwrap();
        wc.load_device(TEST_MNEMONIC, "run1", Some(&dl))
            .await
            .unwrap();

        let wc2 = TrezorWireClient::connect(TREZOR_PORT).await.unwrap();
        let features = wc2.get_features().await.unwrap();
        assert_eq!(extract_features_label(&features).as_deref(), Some("run1"));

        emu.stop().await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Wipe profile to simulate fresh start.
    if profile.exists() {
        std::fs::remove_dir_all(&profile).ok();
    }

    // Second run: should be uninitialized (no label).
    {
        let mut emu = emulators::trezor::TrezorEmulator::new_with_binary(
            bin,
            bundle_dir,
            profile.clone(),
            TREZOR_PORT,
        );
        emu.start().await.unwrap();
        wait_for_wire_ready(TREZOR_PORT).await;

        let wc = TrezorWireClient::connect(TREZOR_PORT).await.unwrap();
        let features = wc.get_features().await.unwrap();
        let label = extract_features_label(&features);
        assert!(
            label.is_none() || label.as_deref() == Some(""),
            "After profile wipe, label should be empty, got: {label:?}"
        );

        emu.stop().await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    std::fs::remove_dir_all(&profile).ok();
}

// ── BitBox02 E2E ────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires BitBox02 bundle installed"]
async fn bitbox02_emulator_starts_and_accepts_tcp() {
    if !bundle_installed(WalletType::BitBox02) {
        eprintln!("SKIP: BitBox02 bundle not installed");
        return;
    }

    let mgr = bundle_manager();
    let bin = mgr
        .emulator_binary_path(WalletType::BitBox02)
        .expect("BitBox02 binary not found");
    let bundle_dir = bin.parent().unwrap().to_path_buf();

    let mut emu: Box<dyn Emulator> = Box::new(
        emulators::generic::GenericEmulator::new(
            WalletType::BitBox02,
            bin,
            bundle_dir,
            std::path::PathBuf::from("/tmp/hwwtui-test-bitbox02"),
            TransportConfig::Tcp {
                host: "127.0.0.1".into(),
                port: 15423,
            },
        )
        .with_arg("--port")
        .with_arg("15423"),
    );

    emu.start().await.expect("BitBox02 start failed");
    assert_eq!(emu.status(), EmulatorStatus::Running);

    // Verify TCP connection actually works (not just the probe).
    let stream = tokio::net::TcpStream::connect("127.0.0.1:15423")
        .await
        .expect("TCP connect to BitBox02 simulator failed");
    assert!(
        stream.peer_addr().is_ok(),
        "Should have a valid peer address"
    );
    drop(stream);

    // Health check should still pass after our connection (RST probe fix).
    assert!(
        emu.health_check().await,
        "Health check should pass after TCP connection"
    );

    emu.stop().await.unwrap();
    assert_eq!(emu.status(), EmulatorStatus::Stopped);
}

#[tokio::test]
#[ignore = "requires BitBox02 bundle + /dev/uhid"]
async fn bitbox02_discoverable_via_uhid_bridge() {
    if !bundle_installed(WalletType::BitBox02) || !uhid_available() {
        eprintln!("SKIP: BitBox02 bundle not installed or /dev/uhid not available");
        return;
    }

    let mgr = bundle_manager();
    let bin = mgr
        .emulator_binary_path(WalletType::BitBox02)
        .expect("BitBox02 binary not found");
    let bundle_dir = bin.parent().unwrap().to_path_buf();

    let mut emu: Box<dyn Emulator> = Box::new(
        emulators::generic::GenericEmulator::new(
            WalletType::BitBox02,
            bin,
            bundle_dir,
            std::path::PathBuf::from("/tmp/hwwtui-test-bitbox02-bridge"),
            TransportConfig::Tcp {
                host: "127.0.0.1".into(),
                port: 15423,
            },
        )
        .with_arg("--port")
        .with_arg("15423"),
    );

    emu.start().await.expect("BitBox02 start failed");

    use bridge::generic::{BridgeTransport, GenericBridge, GenericBridgeConfig};
    use bridge::uhid::{BITBOX02_HID_REPORT_DESCRIPTOR, BITBOX02_PID, BITBOX02_VID};
    use bridge::Bridge;

    let cfg = GenericBridgeConfig::new(
        BITBOX02_VID,
        BITBOX02_PID,
        "BitBox02",
        BITBOX02_HID_REPORT_DESCRIPTOR,
        BridgeTransport::Tcp {
            host: "127.0.0.1".into(),
            port: 15423,
        },
    );
    let mut bridge = GenericBridge::new(cfg);
    let _rx = bridge.start().await.expect("Bridge start failed");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify hidapi discovers the virtual device with correct VID/PID.
    let api = hidapi::HidApi::new().expect("hidapi init failed");
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == BITBOX02_VID && d.product_id() == BITBOX02_PID);
    assert!(
        found,
        "BitBox02 should be discoverable via hidapi after bridge start"
    );

    // Verify product string (sigvault-desktop checks this).
    let has_name = api.device_list().any(|d| {
        d.vendor_id() == BITBOX02_VID
            && d.product_id() == BITBOX02_PID
            && d.product_string()
                .map(|s| s.contains("BitBox02"))
                .unwrap_or(false)
    });
    assert!(
        has_name,
        "BitBox02 UHID device should have 'BitBox02' in product string"
    );

    bridge.stop().await.unwrap();
    emu.stop().await.unwrap();
}

// ── Coldcard E2E ────────────────────────────────────────────────────────────

/// Build a Coldcard [`GenericEmulator`] pointed at the installed bundle.
///
/// The Coldcard simulator is a MicroPython binary that opens a DGRAM Unix
/// socket at `/tmp/ckcc-simulator.sock` to communicate with HID clients.
fn build_coldcard_emulator(socket_path: &std::path::Path) -> Box<dyn Emulator> {
    let mgr = bundle_manager();
    let bin = mgr
        .emulator_binary_path(WalletType::Coldcard)
        .expect("Coldcard binary not found");
    let bundle_dir = bin.parent().unwrap().to_path_buf();
    let shared_dir = bundle_dir.join("shared");
    let unix_dir = bundle_dir.join("unix");
    let work_dir = unix_dir.join("work");
    let sim_boot = unix_dir.join("sim_boot.py");

    // Clean up stale socket.
    std::fs::remove_file(socket_path).ok();

    for sub in &["MicroSD", "settings", "VirtDisk", "debug"] {
        std::fs::create_dir_all(work_dir.join(sub)).ok();
    }

    // Launch via bash to open /dev/null file descriptors for the
    // simulator's display/LED/data pipes.
    let bash_cmd = format!(
        "exec 10>/dev/null 11>/dev/null 12>/dev/null; exec {} -X heapsize=9m {} 10 -1 11 12 {}",
        bin.display(),
        sim_boot.display(),
        socket_path.display(),
    );

    Box::new(
        emulators::generic::GenericEmulator::new(
            WalletType::Coldcard,
            std::path::PathBuf::from("/bin/bash"),
            work_dir,
            std::path::PathBuf::from("/tmp/hwwtui-test-coldcard"),
            TransportConfig::UnixSocket {
                path: socket_path.to_path_buf(),
            },
        )
        .with_env("MICROPYPATH", &format!(":{}", shared_dir.display()))
        .with_arg("-c")
        .with_arg(&bash_cmd),
    )
}

#[tokio::test]
#[ignore = "requires Coldcard bundle installed"]
async fn coldcard_emulator_starts_and_socket_exists() {
    if !bundle_installed(WalletType::Coldcard) {
        eprintln!("SKIP: Coldcard bundle not installed");
        return;
    }

    let socket_path = std::path::PathBuf::from("/tmp/ckcc-simulator.sock");
    std::fs::remove_file(&socket_path).ok();

    let mut emu = build_coldcard_emulator(&socket_path);

    emu.start().await.expect("Coldcard start failed");
    assert_eq!(emu.status(), EmulatorStatus::Running);

    assert!(socket_path.exists(), "Coldcard DGRAM socket should exist");

    // Verify DGRAM connectivity: connect an unbound datagram socket and send
    // a 64-byte zero-padded report.  The server won't respond to garbage data,
    // but a successful send proves the socket is live and accepting datagrams.
    let client =
        tokio::net::UnixDatagram::unbound().expect("Failed to create unbound UnixDatagram");
    client
        .connect(&socket_path)
        .expect("UnixDatagram::connect to Coldcard socket failed");

    let report = vec![0u8; 64];
    client
        .send(&report)
        .await
        .expect("Failed to send test datagram to Coldcard socket");

    emu.stop().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Coldcard bundle + /dev/uhid"]
async fn coldcard_discoverable_via_uhid_bridge() {
    if !bundle_installed(WalletType::Coldcard) || !uhid_available() {
        eprintln!("SKIP: Coldcard bundle not installed or /dev/uhid not available");
        return;
    }

    let socket_path = std::path::PathBuf::from("/tmp/ckcc-simulator.sock");
    std::fs::remove_file(&socket_path).ok();

    let mut emu = build_coldcard_emulator(&socket_path);
    emu.start().await.expect("Coldcard start failed");

    use bridge::generic::{BridgeTransport, GenericBridge, GenericBridgeConfig};
    use bridge::uhid::{COLDCARD_HID_REPORT_DESCRIPTOR, COLDCARD_PID, COLDCARD_VID};
    use bridge::Bridge;

    let cfg = GenericBridgeConfig::new(
        COLDCARD_VID,
        COLDCARD_PID,
        "Coldcard",
        COLDCARD_HID_REPORT_DESCRIPTOR,
        BridgeTransport::UnixDgram {
            path: socket_path.clone(),
        },
    );
    let mut bridge = GenericBridge::new(cfg);
    let _rx = bridge.start().await.expect("Coldcard bridge start failed");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify hidapi discovers the virtual device with correct VID/PID.
    let api = hidapi::HidApi::new().expect("hidapi init failed");
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == COLDCARD_VID && d.product_id() == COLDCARD_PID);
    assert!(
        found,
        "Coldcard should be discoverable via hidapi after bridge start"
    );

    // Verify the product string matches what sigvault-desktop expects.
    let has_name = api.device_list().any(|d| {
        d.vendor_id() == COLDCARD_VID
            && d.product_id() == COLDCARD_PID
            && d.product_string()
                .map(|s| s.contains("Coldcard"))
                .unwrap_or(false)
    });
    assert!(
        has_name,
        "Coldcard UHID device should have 'Coldcard' in product string"
    );

    bridge.stop().await.unwrap();
    emu.stop().await.unwrap();
}

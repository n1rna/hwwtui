//! BitBox02 end-to-end test.
//!
//! Exercises the full flow that sigvault-desktop uses:
//! 1. Start BitBox02 emulator
//! 2. Initialize it via direct TCP (bitbox-api simulator mode)
//! 3. Start UHID bridge
//! 4. Open via hidapi + communicate via bitbox-api (exactly like desktop app)
//!
//! ```bash
//! cargo test -p bridge --test bitbox02_e2e -- --ignored --nocapture
//! ```

use std::time::Duration;

use bridge::generic::{BridgeTransport, GenericBridge, GenericBridgeConfig};
use bridge::uhid::{BITBOX02_HID_REPORT_DESCRIPTOR, BITBOX02_PID, BITBOX02_VID};
use bridge::Bridge;
use bundler::BundleManager;
use emulators::{Emulator, EmulatorStatus, TransportConfig, WalletType};

fn bundle_installed() -> bool {
    let mgr = BundleManager::new("n1rna/hwwtui").unwrap();
    matches!(
        mgr.status(WalletType::BitBox02),
        bundler::BundleStatus::Installed { .. }
    )
}

fn uhid_available() -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uhid")
        .is_ok()
}

async fn start_bitbox02_emulator() -> Box<dyn Emulator> {
    let mgr = BundleManager::new("n1rna/hwwtui").unwrap();
    let bin = mgr
        .emulator_binary_path(WalletType::BitBox02)
        .expect("BitBox02 binary not found");
    let bundle_dir = bin.parent().unwrap().to_path_buf();

    let mut emu: Box<dyn Emulator> = Box::new(
        emulators::generic::GenericEmulator::new(
            WalletType::BitBox02,
            bin,
            bundle_dir,
            std::path::PathBuf::from("/tmp/hwwtui-test-bb02-e2e"),
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
    emu
}

/// Test 1: bitbox-api can connect to simulator via TCP, pair, and initialize.
#[tokio::test]
#[ignore = "requires BitBox02 bundle"]
async fn bitbox02_simulator_tcp_init_and_device_info() {
    if !bundle_installed() {
        eprintln!("SKIP: BitBox02 bundle not installed");
        return;
    }

    let mut emu = start_bitbox02_emulator().await;

    let noise_config = Box::new(bitbox_api::NoiseConfigNoCache {});
    let bitbox = bitbox_api::BitBox::<bitbox_api::runtime::TokioRuntime>::from_simulator(
        None,
        noise_config,
    )
    .await
    .expect("from_simulator failed");

    let pairing = bitbox.unlock_and_pair().await.expect("unlock_and_pair failed");
    let paired = pairing.wait_confirm().await.expect("wait_confirm failed");

    paired
        .restore_from_mnemonic()
        .await
        .expect("restore_from_mnemonic failed");

    let info = paired.device_info().await.expect("device_info failed");
    assert!(info.initialized, "Device should be initialized");
    eprintln!("PASS: name={:?}, initialized={}", info.name, info.initialized);

    drop(paired);
    emu.stop().await.unwrap();
}

/// Test 2: Full E2E — init via TCP, then bridge, then communicate via UHID/hidapi
/// exactly like sigvault-desktop does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires BitBox02 bundle + /dev/uhid + hidraw permissions"]
async fn bitbox02_full_e2e_via_uhid_bridge() {
    if !bundle_installed() || !uhid_available() {
        eprintln!("SKIP: BitBox02 bundle not installed or /dev/uhid not available");
        return;
    }

    // ── Phase 1: Start emulator and initialize via direct TCP ──────────
    let mut emu = start_bitbox02_emulator().await;

    eprintln!("Phase 1: Initializing simulator via TCP...");
    {
        let noise_config = Box::new(bitbox_api::NoiseConfigNoCache {});
        let bitbox = bitbox_api::BitBox::<bitbox_api::runtime::TokioRuntime>::from_simulator(
            None,
            noise_config,
        )
        .await
        .expect("from_simulator failed");
        let pairing = bitbox.unlock_and_pair().await.expect("unlock failed");
        let paired = pairing.wait_confirm().await.expect("confirm failed");
        paired.restore_from_mnemonic().await.expect("restore failed");
        let info = paired.device_info().await.expect("device_info failed");
        assert!(info.initialized);
        eprintln!("  Initialized: name={:?}", info.name);
    }
    // TCP connection dropped here.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ── Phase 2: Start UHID bridge ─────────────────────────────────────
    eprintln!("Phase 2: Starting UHID bridge...");
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
    let _rx = bridge.start().await.expect("bridge start failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ── Phase 3: Discover via hidapi (like sigvault-desktop) ───────────
    eprintln!("Phase 3: hidapi discovery...");
    let api = hidapi::HidApi::new().expect("hidapi init failed");
    let device_info = api
        .device_list()
        .find(|d| d.vendor_id() == BITBOX02_VID && d.product_id() == BITBOX02_PID)
        .expect("BitBox02 not found in HID list");
    eprintln!("  Found at {:?}", device_info.path());

    let hid_device = match device_info.open_device(&api) {
        Ok(d) => d,
        Err(e) => {
            let path_str = device_info.path().to_str().unwrap_or("?");
            if let Ok(meta) = std::fs::metadata(path_str) {
                use std::os::unix::fs::PermissionsExt;
                eprintln!("  hidraw permissions: {:o}", meta.permissions().mode());
            }
            panic!("Cannot open hidraw: {e}. Run: just setup-udev");
        }
    };
    eprintln!("  Opened HID device successfully");

    // ── Phase 4: Communicate via bitbox-api through UHID ───────────────
    eprintln!("Phase 4: bitbox-api via UHID bridge...");
    let noise_config = Box::new(bitbox_api::NoiseConfigNoCache {});
    let bitbox =
        bitbox_api::BitBox::<bitbox_api::runtime::TokioRuntime>::from_hid_device(
            hid_device,
            noise_config,
        )
        .await
        .expect("from_hid_device failed");

    eprintln!("  unlock_and_pair...");
    let pairing = bitbox.unlock_and_pair().await.expect("unlock failed via UHID");

    eprintln!("  wait_confirm...");
    let paired = pairing.wait_confirm().await.expect("confirm failed via UHID");

    eprintln!("  device_info...");
    let info = paired.device_info().await.expect("device_info failed via UHID");
    assert!(info.initialized, "Device should be initialized via UHID");
    eprintln!("  Device: name={:?}, initialized={}", info.name, info.initialized);

    eprintln!("PASS: Full BitBox02 E2E via UHID bridge!");

    drop(paired);
    bridge.stop().await.unwrap();
    emu.stop().await.unwrap();
}

//! Reproduction of the full TUI BitBox02 `l` key flow.
//!
//! Simulates exactly what happens when user presses `s` then `l`:
//! 1. Start emulator + bridge (press `s`)
//! 2. Stop bridge, stop emulator, restart emulator, init, restart bridge (press `l`)
//! 3. Desktop app connects via hidapi through UHID bridge
//!
//! Run: cargo test -p bridge --test bb02_repro -- --ignored --nocapture

use bridge::generic::{BridgeTransport, GenericBridge, GenericBridgeConfig};
use bridge::uhid::{BITBOX02_HID_REPORT_DESCRIPTOR, BITBOX02_PID, BITBOX02_VID};
use bridge::Bridge;
use bundler::BundleManager;
use emulators::{Emulator, EmulatorStatus, TransportConfig, WalletType};
use std::path::PathBuf;
use std::time::Duration;

fn bb02_bridge_config() -> GenericBridgeConfig {
    GenericBridgeConfig::new(
        BITBOX02_VID,
        BITBOX02_PID,
        "BitBox02",
        BITBOX02_HID_REPORT_DESCRIPTOR,
        BridgeTransport::Tcp {
            host: "127.0.0.1".into(),
            port: 15423,
        },
    )
}

fn new_emulator() -> Box<dyn Emulator> {
    let mgr = BundleManager::new("n1rna/hwwtui").unwrap();
    let bin = mgr.emulator_binary_path(WalletType::BitBox02).unwrap();
    let bundle_dir = bin.parent().unwrap().to_path_buf();
    Box::new(
        emulators::generic::GenericEmulator::new(
            WalletType::BitBox02,
            bin,
            bundle_dir,
            PathBuf::from("/tmp/hwwtui-repro-bb02"),
            TransportConfig::Tcp {
                host: "127.0.0.1".into(),
                port: 15423,
            },
        )
        .with_arg("--port")
        .with_arg("15423"),
    )
}

/// Simulates the `s` key: start emulator + bridge
async fn simulate_s_key() -> (Box<dyn Emulator>, GenericBridge) {
    eprintln!("[s] Starting emulator...");
    let mut emu = new_emulator();
    emu.start().await.unwrap();
    assert_eq!(emu.status(), EmulatorStatus::Running);

    eprintln!("[s] Starting bridge...");
    let mut bridge = GenericBridge::new(bb02_bridge_config());
    let _rx = bridge.start().await.unwrap();
    eprintln!("[s] Done — emulator + bridge running");
    (emu, bridge)
}

/// Simulates the `l` key: stop bridge, restart emu, init, restart bridge
async fn simulate_l_key(emu: &mut Box<dyn Emulator>, bridge: &mut Option<GenericBridge>) {
    eprintln!("[l] Stopping bridge...");
    if let Some(b) = bridge.as_mut() {
        b.stop().await.ok();
    }
    *bridge = None;

    eprintln!("[l] Stopping emulator...");
    emu.stop().await.ok();
    tokio::time::sleep(Duration::from_millis(500)).await;

    eprintln!("[l] Restarting emulator...");
    emu.start().await.unwrap();

    eprintln!("[l] Initializing via bitbox-api (separate thread)...");
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(async {
            let nc = Box::new(bitbox_api::NoiseConfigNoCache {});
            let bb =
                bitbox_api::BitBox::<bitbox_api::runtime::TokioRuntime>::from_simulator(None, nc)
                    .await
                    .map_err(|e| format!("connect: {e:?}"))?;
            let pb = bb
                .unlock_and_pair()
                .await
                .map_err(|e| format!("pair: {e:?}"))?;
            let paired = pb
                .wait_confirm()
                .await
                .map_err(|e| format!("confirm: {e:?}"))?;
            paired
                .restore_from_mnemonic()
                .await
                .map_err(|e| format!("restore: {e:?}"))?;
            Ok::<(), String>(())
        });
        let _ = tx.send(result);
    });
    let result = rx.await;

    match &result {
        Ok(Ok(())) => eprintln!("[l] Initialized OK"),
        Ok(Err(e)) => panic!("[l] Init failed: {e}"),
        Err(_) => panic!("[l] Init thread died"),
    }

    eprintln!("[l] Waiting for TCP cleanup...");
    tokio::time::sleep(Duration::from_secs(1)).await;

    eprintln!("[l] Restarting bridge...");
    let mut new_bridge = GenericBridge::new(bb02_bridge_config());
    let _rx = new_bridge.start().await.unwrap();
    *bridge = Some(new_bridge);
    eprintln!("[l] Done — emulator initialized + bridge running");
}

/// Simulates sigvault-desktop discovery
async fn simulate_desktop_discovery() {
    // Give kernel time to register the new hidraw device.
    tokio::time::sleep(Duration::from_millis(500)).await;
    eprintln!("[desktop] Discovering via hidapi...");
    let api = hidapi::HidApi::new().unwrap();
    let dev_info = api
        .device_list()
        .find(|d| d.vendor_id() == BITBOX02_VID && d.product_id() == BITBOX02_PID)
        .expect("[desktop] BitBox02 not found in HID list");
    let hid_device = dev_info
        .open_device(&api)
        .expect("[desktop] Can't open hidraw");
    eprintln!("[desktop] Opened {:?}", dev_info.path());

    eprintln!("[desktop] from_hid_device...");
    let nc = Box::new(bitbox_api::NoiseConfigNoCache {});
    let bb =
        bitbox_api::BitBox::<bitbox_api::runtime::TokioRuntime>::from_hid_device(hid_device, nc)
            .await
            .expect("[desktop] from_hid_device failed");
    eprintln!("[desktop] unlock_and_pair...");
    let pb = bb.unlock_and_pair().await.expect("[desktop] unlock failed");
    eprintln!("[desktop] wait_confirm...");
    let paired = pb.wait_confirm().await.expect("[desktop] confirm failed");
    eprintln!("[desktop] device_info...");
    let info = paired
        .device_info()
        .await
        .expect("[desktop] device_info failed");
    eprintln!(
        "[desktop] name={:?} initialized={}",
        info.name, info.initialized
    );
    assert!(info.initialized, "Device should be initialized");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn repro_full_tui_then_desktop_flow() {
    // User presses `s`
    let (mut emu, bridge) = simulate_s_key().await;
    let mut bridge = Some(bridge);

    // User presses `l`
    simulate_l_key(&mut emu, &mut bridge).await;

    // User opens sigvault-desktop and triggers discovery
    let result = tokio::time::timeout(Duration::from_secs(10), simulate_desktop_discovery()).await;

    match result {
        Ok(()) => eprintln!("\n*** PASS ***"),
        Err(_) => panic!("\n*** FAIL: desktop discovery timed out ***"),
    }

    if let Some(mut b) = bridge {
        b.stop().await.ok();
    }
    emu.stop().await.ok();
}

/// Same test but with a 30-second delay between bridge start and discovery
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn repro_with_30s_idle_delay() {
    let (mut emu, bridge) = simulate_s_key().await;
    let mut bridge = Some(bridge);
    simulate_l_key(&mut emu, &mut bridge).await;

    eprintln!("[delay] Waiting 30 seconds before discovery...");
    tokio::time::sleep(Duration::from_secs(30)).await;
    eprintln!("[delay] Done waiting");

    let result = tokio::time::timeout(Duration::from_secs(10), simulate_desktop_discovery()).await;

    match result {
        Ok(()) => eprintln!("\n*** PASS ***"),
        Err(_) => panic!("\n*** FAIL: desktop discovery timed out after 30s idle ***"),
    }

    if let Some(mut b) = bridge {
        b.stop().await.ok();
    }
    emu.stop().await.ok();
}

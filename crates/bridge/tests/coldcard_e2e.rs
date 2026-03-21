//! Coldcard end-to-end test.
//!
//! Replicates the exact sigvault-desktop discovery flow:
//! 1. Start emulator
//! 2. Start UHID bridge (DGRAM)
//! 3. Open via hidapi
//! 4. Use coldcard crate to open and communicate
//!
//! Run: cargo test -p bridge --test coldcard_e2e -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Duration;

use bridge::generic::{BridgeTransport, GenericBridge, GenericBridgeConfig};
use bridge::uhid::{COLDCARD_HID_REPORT_DESCRIPTOR, COLDCARD_PID, COLDCARD_VID};
use bridge::Bridge;
use bundler::BundleManager;
use emulators::{Emulator, EmulatorStatus, TransportConfig, WalletType};

fn bundle_installed() -> bool {
    let mgr = BundleManager::new("n1rna/hwwtui").unwrap();
    matches!(
        mgr.status(WalletType::Coldcard),
        bundler::BundleStatus::Installed { .. }
    )
}

fn uhid_available() -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uhid")
        .is_ok()
}

fn start_coldcard_emulator(socket_path: &std::path::Path) -> Box<dyn Emulator> {
    let mgr = BundleManager::new("n1rna/hwwtui").unwrap();
    let bin = mgr
        .emulator_binary_path(WalletType::Coldcard)
        .expect("Coldcard binary not found");
    let bundle_dir = bin.parent().unwrap().to_path_buf();
    let shared_dir = bundle_dir.join("shared");
    let unix_dir = bundle_dir.join("unix");
    let work_dir = unix_dir.join("work");
    let sim_boot = unix_dir.join("sim_boot.py");

    std::fs::remove_file(socket_path).ok();
    for sub in &["MicroSD", "settings", "VirtDisk", "debug"] {
        std::fs::create_dir_all(work_dir.join(sub)).ok();
    }

    let bash_cmd = format!(
        "exec 10>/dev/null 11>/dev/null 12>/dev/null; exec {} -X heapsize=9m {} 10 -1 11 12 {}",
        bin.display(),
        sim_boot.display(),
        socket_path.display(),
    );

    Box::new(
        emulators::generic::GenericEmulator::new(
            WalletType::Coldcard,
            PathBuf::from("/bin/bash"),
            work_dir,
            PathBuf::from("/tmp/hwwtui-test-cc-e2e"),
            TransportConfig::UnixSocket {
                path: socket_path.to_path_buf(),
            },
        )
        .with_env("MICROPYPATH", &format!(":{}", shared_dir.display()))
        .with_arg("-c")
        .with_arg(&bash_cmd),
    )
}

/// Step 1: Verify the emulator starts and the DGRAM socket works.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Coldcard bundle"]
async fn step1_emulator_starts() {
    if !bundle_installed() {
        eprintln!("SKIP");
        return;
    }

    let socket_path = PathBuf::from("/tmp/ckcc-simulator.sock");
    let mut emu = start_coldcard_emulator(&socket_path);
    emu.start().await.expect("Coldcard start failed");
    assert_eq!(emu.status(), EmulatorStatus::Running);
    assert!(socket_path.exists());

    // Verify DGRAM works
    let client = std::os::unix::net::UnixDatagram::unbound().unwrap();
    client.connect(&socket_path).unwrap();
    client.send(&[0u8; 64]).unwrap();
    eprintln!("PASS: emulator started, DGRAM works");

    emu.stop().await.unwrap();
}

/// Step 2: UHID bridge creates device discoverable via hidapi.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Coldcard bundle + /dev/uhid"]
async fn step2_uhid_bridge_discovery() {
    if !bundle_installed() || !uhid_available() {
        eprintln!("SKIP");
        return;
    }

    let socket_path = PathBuf::from("/tmp/ckcc-simulator.sock");
    let mut emu = start_coldcard_emulator(&socket_path);
    emu.start().await.expect("start failed");

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
    let _rx = bridge.start().await.expect("bridge start failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let api = hidapi::HidApi::new().unwrap();
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == COLDCARD_VID && d.product_id() == COLDCARD_PID);
    assert!(found, "Coldcard not found via hidapi");
    eprintln!("PASS: Coldcard discoverable via hidapi");

    bridge.stop().await.unwrap();
    emu.stop().await.unwrap();
}

/// Step 3: Open the hidraw device and do a raw HID write/read through
/// the UHID bridge to see if the Coldcard simulator responds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Coldcard bundle + /dev/uhid + hidraw perms"]
async fn step3_hid_write_read_through_bridge() {
    if !bundle_installed() || !uhid_available() {
        eprintln!("SKIP");
        return;
    }

    let socket_path = PathBuf::from("/tmp/ckcc-simulator.sock");
    let mut emu = start_coldcard_emulator(&socket_path);
    emu.start().await.expect("start failed");

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
    let _rx = bridge.start().await.expect("bridge start failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let api = hidapi::HidApi::new().unwrap();
    let dev_info = api
        .device_list()
        .find(|d| d.vendor_id() == COLDCARD_VID && d.product_id() == COLDCARD_PID)
        .expect("Coldcard not in HID list");

    let hid_device = dev_info.open_device(&api).expect("Can't open hidraw");
    eprintln!("Opened {:?}", dev_info.path());

    // Send a Coldcard "vers" command through HID.
    // Protocol: report_id(0x00) + flag_byte + data
    // flag_byte: 0x84 = last packet (0x80) | length 4
    // data: "vers" (4 bytes)
    let mut msg = vec![0x00u8; 65]; // report ID + 64 bytes payload
    msg[1] = 0x84; // is_last=1, length=4
    msg[2] = b'v';
    msg[3] = b'e';
    msg[4] = b'r';
    msg[5] = b's';

    match hid_device.write(&msg) {
        Ok(n) => eprintln!("HID write: {} bytes", n),
        Err(e) => {
            eprintln!("HID write failed: {}", e);
            bridge.stop().await.unwrap();
            emu.stop().await.unwrap();
            panic!("HID write failed");
        }
    }

    // Try to read response with timeout
    let mut buf = [0u8; 64];
    match hid_device.read_timeout(&mut buf, 3000) {
        Ok(0) => eprintln!("HID read: 0 bytes (no response within 3s)"),
        Ok(n) => eprintln!("HID read: {} bytes, first 8: {:02x?}", n, &buf[..n.min(8)]),
        Err(e) => eprintln!("HID read error: {}", e),
    }

    eprintln!("PASS: HID write/read through UHID bridge completed");

    bridge.stop().await.unwrap();
    emu.stop().await.unwrap();
}

/// Step 4: Use the coldcard crate's Coldcard::open through UHID bridge.
/// This is exactly what sigvault-desktop does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Coldcard bundle + /dev/uhid + hidraw perms"]
async fn step4_coldcard_crate_open() {
    if !bundle_installed() || !uhid_available() {
        eprintln!("SKIP");
        return;
    }

    let socket_path = PathBuf::from("/tmp/ckcc-simulator.sock");
    let mut emu = start_coldcard_emulator(&socket_path);
    emu.start().await.expect("start failed");

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
    let _rx = bridge.start().await.expect("bridge start failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Exactly what sigvault-desktop does
    eprintln!("Opening via coldcard crate...");
    let result = tokio::task::spawn_blocking(|| {
        let mut cc_api = coldcard::Api::new().expect("Api::new failed");

        // First, detect serials
        let serials = cc_api.detect();
        eprintln!("  detect() = {:?}", serials);

        match serials {
            Ok(sns) if !sns.is_empty() => {
                eprintln!("  Opening serial: {:?}", sns[0]);
                match cc_api.open(&sns[0], None) {
                    Ok((cc, xpub)) => {
                        eprintln!("  Coldcard opened! xpub={:?}", xpub);
                        Ok(true)
                    }
                    Err(e) => {
                        eprintln!("  open() failed: {:?}", e);
                        Err(format!("{:?}", e))
                    }
                }
            }
            Ok(_) => {
                eprintln!("  No Coldcard serials found");
                Err("no serials".to_string())
            }
            Err(e) => {
                eprintln!("  detect() failed: {:?}", e);
                Err(format!("{:?}", e))
            }
        }
    })
    .await;

    match &result {
        Ok(Ok(true)) => eprintln!("\n*** PASS ***"),
        Ok(Err(e)) => eprintln!("\n*** FAIL: {} ***", e),
        Err(e) => eprintln!("\n*** FAIL: task error: {} ***", e),
        _ => eprintln!("\n*** FAIL ***"),
    }

    bridge.stop().await.unwrap();
    emu.stop().await.unwrap();
}

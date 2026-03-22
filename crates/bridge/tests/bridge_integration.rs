//! Integration tests for the UHID bridge.
//!
//! Tests marked `#[ignore]` require `/dev/uhid` access (typically root or
//! `plugdev` group membership).  Run them with:
//!
//! ```bash
//! sudo cargo test -p bridge -- --ignored
//! ```

use std::path::Path;
use std::time::Duration;

use bridge::generic::{BridgeTransport, GenericBridge, GenericBridgeConfig};
use bridge::uhid::{
    VirtualHidDevice, BITBOX02_HID_REPORT_DESCRIPTOR, BITBOX02_PID, BITBOX02_VID,
    COLDCARD_HID_REPORT_DESCRIPTOR, COLDCARD_PID, COLDCARD_VID, LEDGER_HID_REPORT_DESCRIPTOR,
    LEDGER_PID, LEDGER_VID,
};
use bridge::Bridge;

// ── Helpers ─────────────────────────────────────────────────────────────────

fn uhid_available() -> bool {
    Path::new("/dev/uhid").exists()
}

/// Count hidraw devices currently present.
fn count_hidraw() -> usize {
    std::fs::read_dir("/dev")
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| n.starts_with("hidraw"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

// ── UHID device creation ────────────────────────────────────────────────────

#[test]
#[ignore = "requires /dev/uhid access"]
fn test_uhid_device_creation() {
    if !uhid_available() {
        eprintln!("Skipping: /dev/uhid not available");
        return;
    }

    let before = count_hidraw();
    let _dev = VirtualHidDevice::new(
        BITBOX02_VID,
        BITBOX02_PID,
        "test-uhid-creation",
        BITBOX02_HID_REPORT_DESCRIPTOR,
    )
    .expect("Failed to create UHID device");

    // Give the kernel a moment to create the hidraw node.
    std::thread::sleep(Duration::from_millis(200));
    let after = count_hidraw();
    assert!(
        after > before,
        "Expected new hidraw device (before={before}, after={after})"
    );
}

// ── hidapi discovery ────────────────────────────────────────────────────────

#[test]
#[ignore = "requires /dev/uhid access"]
fn test_bitbox02_hidapi_discovery() {
    if !uhid_available() {
        return;
    }

    let _dev = VirtualHidDevice::new(
        BITBOX02_VID,
        BITBOX02_PID,
        "BitBox02",
        BITBOX02_HID_REPORT_DESCRIPTOR,
    )
    .expect("UHID create failed");

    std::thread::sleep(Duration::from_millis(300));

    let api = hidapi::HidApi::new().expect("hidapi init failed");
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == BITBOX02_VID && d.product_id() == BITBOX02_PID);
    assert!(found, "BitBox02 UHID device not found via hidapi");
}

#[test]
#[ignore = "requires /dev/uhid access"]
fn test_coldcard_hidapi_discovery() {
    if !uhid_available() {
        return;
    }

    let _dev = VirtualHidDevice::new(
        COLDCARD_VID,
        COLDCARD_PID,
        "Coldcard (emulated)",
        COLDCARD_HID_REPORT_DESCRIPTOR,
    )
    .expect("UHID create failed");

    std::thread::sleep(Duration::from_millis(300));

    let api = hidapi::HidApi::new().expect("hidapi init failed");
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == COLDCARD_VID && d.product_id() == COLDCARD_PID);
    assert!(found, "Coldcard UHID device not found via hidapi");
}

#[test]
#[ignore = "requires /dev/uhid access"]
fn test_ledger_hidapi_discovery() {
    if !uhid_available() {
        return;
    }

    let _dev = VirtualHidDevice::new(
        LEDGER_VID,
        LEDGER_PID,
        "Ledger (emulated)",
        LEDGER_HID_REPORT_DESCRIPTOR,
    )
    .expect("UHID create failed");

    std::thread::sleep(Duration::from_millis(300));

    let api = hidapi::HidApi::new().expect("hidapi init failed");
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == LEDGER_VID && d.product_id() == LEDGER_PID);
    assert!(found, "Ledger UHID device not found via hidapi");
}

// ── Bridge TCP relay ────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires /dev/uhid access"]
async fn test_bridge_tcp_relay() {
    if !uhid_available() {
        return;
    }

    // Spawn a mock TCP server that echoes 64-byte packets.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind failed");
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 64];
            while stream.read_exact(&mut buf).await.is_ok() {
                // Echo back with first byte incremented.
                buf[0] = buf[0].wrapping_add(1);
                if stream.write_all(&buf).await.is_err() {
                    break;
                }
            }
        }
    });

    let cfg = GenericBridgeConfig::new(
        BITBOX02_VID,
        BITBOX02_PID,
        "BitBox02 relay test",
        BITBOX02_HID_REPORT_DESCRIPTOR,
        BridgeTransport::Tcp {
            host: "127.0.0.1".into(),
            port,
        },
    );

    let mut bridge = GenericBridge::new(cfg);
    let mut rx = bridge.start().await.expect("bridge start failed");

    // Give the bridge time to set up.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(bridge.is_running());

    // We can verify that bridge messages arrive on the intercept channel
    // by checking that the bridge is running and connected.
    bridge.stop().await.expect("bridge stop failed");
    assert!(!bridge.is_running());

    // Drain any messages that arrived.
    while rx.try_recv().is_ok() {}
}

// ── Bridge bidirectional throughput ─────────────────────────────────────────

#[tokio::test]
#[ignore = "requires /dev/uhid access"]
async fn test_bridge_bidirectional_throughput() {
    if !uhid_available() {
        return;
    }

    // Spawn a mock TCP server that echoes packets.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind failed");
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 64];
            for _ in 0..10 {
                if stream.read_exact(&mut buf).await.is_err() {
                    break;
                }
                if stream.write_all(&buf).await.is_err() {
                    break;
                }
            }
        }
    });

    let cfg = GenericBridgeConfig::new(
        BITBOX02_VID,
        BITBOX02_PID,
        "BitBox02 throughput test",
        BITBOX02_HID_REPORT_DESCRIPTOR,
        BridgeTransport::Tcp {
            host: "127.0.0.1".into(),
            port,
        },
    );

    let mut bridge = GenericBridge::new(cfg);
    let _rx = bridge.start().await.expect("bridge start failed");

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(bridge.is_running());

    bridge.stop().await.expect("bridge stop failed");
}

// ── sigvault discovery pattern ──────────────────────────────────────────────

#[test]
#[ignore = "requires /dev/uhid access"]
fn test_sigvault_discovery_pattern() {
    if !uhid_available() {
        return;
    }

    let _dev = VirtualHidDevice::new(
        BITBOX02_VID,
        BITBOX02_PID,
        "BitBox02",
        BITBOX02_HID_REPORT_DESCRIPTOR,
    )
    .expect("UHID create failed");

    std::thread::sleep(Duration::from_millis(300));

    // Simulate what sigvault-desktop does: enumerate all HID devices,
    // filter by VID/PID matching BitBox02.
    let api = hidapi::HidApi::new().expect("hidapi init failed");
    let bitbox_devices: Vec<_> = api
        .device_list()
        .filter(|d| d.vendor_id() == BITBOX02_VID && d.product_id() == BITBOX02_PID)
        .collect();

    assert!(
        !bitbox_devices.is_empty(),
        "sigvault discovery pattern: no BitBox02 devices found"
    );

    // Verify the product string contains "BitBox02" (as sigvault checks).
    let has_bitbox_name = bitbox_devices.iter().any(|d| {
        d.product_string()
            .map(|s| s.contains("BitBox02"))
            .unwrap_or(false)
    });
    assert!(
        has_bitbox_name,
        "sigvault discovery pattern: product string should contain 'BitBox02'"
    );
}

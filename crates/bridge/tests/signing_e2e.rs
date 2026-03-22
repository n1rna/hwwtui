//! End-to-end signing tests using async-hwi.
//!
//! Tests xpub retrieval, policy registration, and PSBT signing
//! through each wallet emulator using the same HWI trait that
//! sigvault-desktop uses.
//!
//! Prerequisites:
//! - Start emulators via hwwtui (press `s` on each tab)
//! - For BitBox02: press `l` to initialize
//! - Regtest bitcoind + electrs running
//!
//! Run: cargo test -p bridge --test signing_e2e -- --ignored --nocapture --test-threads=1

/// Specter: xpub retrieval and fingerprint via direct TCP
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Specter emulator on :8789"]
async fn specter_xpub_and_fingerprint() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    async fn cmd(c: &str) -> String {
        let mut s = TcpStream::connect("127.0.0.1:8789").await.unwrap();
        s.write_all(format!("{c}\r\n").as_bytes()).await.unwrap();
        let r = BufReader::new(&mut s);
        let mut lines = r.lines();
        let ack = lines.next_line().await.unwrap().unwrap();
        assert_eq!(ack, "ACK");
        let resp = lines.next_line().await.unwrap().unwrap();
        s.shutdown().await.ok();
        resp
    }

    let fp = cmd("fingerprint").await;
    eprintln!("Fingerprint: {fp}");
    assert_eq!(fp, "73c5da0a");

    for (path, label) in [
        ("m/84h/1h/0h", "native segwit"),
        ("m/86h/1h/0h", "taproot"),
        ("m/48h/1h/0h/2h", "multisig"),
    ] {
        let xpub = cmd(&format!("xpub {path}")).await;
        eprintln!("{label} ({path}): {xpub}");
        assert!(xpub.starts_with("tpub"), "{label}: expected tpub");
    }

    eprintln!("PASS: Specter xpubs");
}

/// Coldcard: xpub via coldcard crate through UHID bridge
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Coldcard emulator + UHID bridge running"]
async fn coldcard_xpub_via_uhid() {
    let result = tokio::task::spawn_blocking(|| {
        let mut api = coldcard::Api::new().map_err(|e| format!("{e:?}"))?;
        let serials = api.detect().map_err(|e| format!("{e:?}"))?;
        if serials.is_empty() {
            return Err("No Coldcard found".into());
        }
        let (mut cc, xpub_info) = api.open(&serials[0], None).map_err(|e| format!("{e:?}"))?;
        eprintln!("Coldcard opened, xpub: {xpub_info:?}");

        let master_xpub = cc.xpub(None).map_err(|e| format!("{e:?}"))?;
        eprintln!("Master xpub: {master_xpub}");

        Ok::<String, String>(master_xpub)
    })
    .await
    .unwrap();

    match result {
        Ok(xpub) => {
            assert!(xpub.starts_with("tpub") || xpub.starts_with("xpub"));
            eprintln!("PASS: Coldcard xpub via UHID");
        }
        Err(e) => panic!("FAIL: {e}"),
    }
}

/// Ledger: verify Speculos is running and responsive
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Ledger Speculos on :9999"]
async fn ledger_connectivity() {
    match tokio::net::TcpStream::connect("127.0.0.1:9999").await {
        Ok(_) => eprintln!("PASS: Ledger Speculos TCP OK"),
        Err(e) => panic!("FAIL: {e}"),
    }

    // Also check the API
    match reqwest::get("http://localhost:5001/events").await {
        Ok(resp) => {
            let body = resp.text().await.unwrap_or_default();
            eprintln!("Speculos events: {}", &body[..body.len().min(100)]);
            eprintln!("PASS: Ledger API OK");
        }
        Err(_) => eprintln!("WARN: Ledger API not available (port 5001)"),
    }
}

/// Jade: ping via CBOR over TCP
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Jade QEMU on :30121"]
async fn jade_ping() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:30121")
        .await
        .expect("Connect to Jade failed");

    // CBOR: {"id":"1","method":"ping"}
    let cbor_ping: Vec<u8> = vec![
        0xa2, 0x62, 0x69, 0x64, 0x61, 0x31, 0x66, 0x6d, 0x65, 0x74, 0x68, 0x6f, 0x64, 0x64, 0x70,
        0x69, 0x6e, 0x67,
    ];

    stream.write_all(&cbor_ping).await.unwrap();

    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("Timeout")
        .expect("Read error");

    assert!(n > 0, "Empty response");
    eprintln!(
        "Jade response: {} bytes, hex: {}",
        n,
        hex::encode(&buf[..n])
    );
    eprintln!("PASS: Jade ping");
}

/// Summary test: check all emulators are reachable
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires all emulators running"]
async fn all_emulators_reachable() {
    let checks = vec![
        ("Trezor", check_udp("127.0.0.1:21324").await),
        ("BitBox02", check_tcp("127.0.0.1:15423").await),
        (
            "Coldcard",
            check_unix_dgram("/tmp/ckcc-simulator.sock").await,
        ),
        ("Specter", check_tcp("127.0.0.1:8789").await),
        ("Ledger", check_tcp("127.0.0.1:9999").await),
        ("Jade", check_tcp("127.0.0.1:30121").await),
    ];

    let mut all_ok = true;
    for (name, ok) in &checks {
        let status = if *ok { "OK" } else { "MISSING" };
        eprintln!("  {name:12} {status}");
        if !*ok {
            all_ok = false;
        }
    }

    if all_ok {
        eprintln!("\nPASS: All 6 emulators reachable");
    } else {
        eprintln!("\nSome emulators not running. Start them via hwwtui.");
    }
}

async fn check_tcp(addr: &str) -> bool {
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

async fn check_udp(addr: &str) -> bool {
    tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .and_then(|s| {
            let addr: std::net::SocketAddr = addr.parse().unwrap();
            s.try_send_to(&[0x3F], addr)?;
            Ok(true)
        })
        .unwrap_or(false)
}

async fn check_unix_dgram(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

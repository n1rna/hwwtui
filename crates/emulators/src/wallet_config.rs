//! Single source of truth for per-wallet configuration.
//!
//! Before this module existed, the same six tuples (binary name, VID, PID,
//! HID report descriptor, transport, port, startup timeout, ...) were
//! re-encoded in four different places:
//!
//! - `crates/hwwtui/src/app.rs` — 275-line match block for emulator
//!   spawning (every wallet's env vars + args + ports).
//! - `crates/bridge/src/uhid.rs` — VID/PID/report-descriptor constants
//!   per wallet, addressed by name in `app.rs`'s bridge-setup match.
//! - `crates/bridge/src/trezor.rs` — Trezor's HID config duplicated.
//! - `crates/bundler/src/lib.rs` + `storage.rs` — binary candidate
//!   names and directory paths.
//!
//! Each of those sites encodes the same per-wallet metadata
//! differently, which is the "messy, not unified" problem the
//! refactor unwinds. This module provides one `WalletConfig` per
//! wallet plus a single `for_wallet()` lookup; downstream sites
//! consume it instead of carrying their own copy.
//!
//! The struct is `pub` + immutable; adding a new wallet is a single
//! const definition plus a row in `ALL_WALLETS`. Removing a field
//! ripples to consumers naturally via the type checker.

use crate::WalletType;

/// Transport variant a host application uses to reach the emulator.
///
/// Distinct from `TransportConfig` (the existing struct that carries
/// concrete host/port values for a running emulator) — this is the
/// *kind* of transport a wallet supports, used at config-load time
/// before any process exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// UDP datagrams on localhost. Trezor.
    Udp,
    /// TCP stream socket. BitBox02 sim, Specter, Ledger (Speculos),
    /// Jade (QEMU). Each uses different ports — see `default_port`.
    Tcp,
    /// Unix stream socket. Currently unused — earlier BitBox02
    /// builds used this transport.
    UnixStream,
    /// Unix datagram socket. Coldcard's simulator opens one at a
    /// fixed path (`/tmp/ckcc-simulator.sock`).
    UnixDgram,
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Udp => write!(f, "UDP"),
            Self::Tcp => write!(f, "TCP"),
            Self::UnixStream => write!(f, "Unix(stream)"),
            Self::UnixDgram => write!(f, "Unix(dgram)"),
        }
    }
}

/// HID descriptor needed to expose an emulator as a virtual USB HID
/// device through Linux UHID. Wallets that don't go through UHID
/// (Trezor — uses UDP directly; Ledger / Jade / Specter — direct
/// TCP) have `hid = None` in their `WalletConfig`.
///
/// Wallets DO that bridge through UHID still need a HID descriptor
/// because the desktop apps use `hidapi` to discover them by VID/PID
/// — those bytes go into the kernel's enumeration so e.g. Sparrow
/// thinks it sees a real BitBox02.
///
/// `report_descriptor` is a static slice — the bytes are HID-spec
/// boilerplate per-vendor that the kernel parses to know what kind
/// of device it's looking at.
#[derive(Debug, Clone, Copy)]
pub struct HidConfig {
    pub vid: u16,
    pub pid: u16,
    pub report_size: usize,
    pub report_descriptor: &'static [u8],
}

/// How the wallet's "load test seed" / "initialize" step runs.
///
/// Some emulators boot empty and need a wire-protocol-level seed
/// injection; some come pre-seeded; some need their own SDK to
/// drive a pairing + restore dance. Modelling this as an enum (vs
/// a closure) keeps `WalletConfig` `const`-able — actual init code
/// lives in the per-wallet trait impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitStyle {
    /// No initialization needed — the simulator boots into a
    /// signed-in state with a deterministic test seed. Coldcard,
    /// Ledger, Jade, Specter.
    Preseeded,
    /// Send a `LoadDevice` wire message over the debug-link UDP
    /// port (main_port + 1). Trezor.
    TrezorWireDebugLink,
    /// Use the `bitbox-api` crate's pairing + restore flow.
    /// BitBox02 only — bitbox-api wraps the noise protocol so we
    /// don't reimplement it.
    BitBoxApi,
}

impl std::fmt::Display for InitStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preseeded => write!(f, "pre-seeded"),
            Self::TrezorWireDebugLink => write!(f, "Trezor debug-link wire"),
            Self::BitBoxApi => write!(f, "BitBox API"),
        }
    }
}

/// Whether the emulator runs natively or inside a Docker container.
/// Drives bundler expectations (Docker wallets ship a runner script
/// instead of a native binary) and platform-availability checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMode {
    /// A native binary or interpreter (Python, MicroPython) on the
    /// host. Trezor, BitBox02, Coldcard, Specter.
    Native,
    /// Wrapped in a Docker container — the bundle ships a runner
    /// script that talks to Docker. Ledger (Speculos), Jade (QEMU).
    /// Useful on macOS too because Docker abstracts the Linux/ARM
    /// gap.
    Docker,
}

/// Single source of truth for one wallet's static config. Built at
/// compile time as a `const` per wallet (see [`ALL_WALLETS`]), so
/// adding a new device is one struct literal away.
#[derive(Debug, Clone, Copy)]
pub struct WalletConfig {
    pub wallet_type: WalletType,
    /// Human-readable name for UI rendering. Same string the
    /// `Display` impl on `WalletType` produces; duplicated here so
    /// the table is self-contained.
    pub display_name: &'static str,
    pub transport: TransportKind,
    pub default_port: u16,
    pub host_mode: HostMode,
    /// `None` when the wallet doesn't need a virtual HID device —
    /// either because it speaks UDP directly (Trezor) or because the
    /// host app discovers it via TCP (Ledger, Jade, Specter).
    pub hid: Option<HidConfig>,
    pub init_style: InitStyle,
    /// Seconds the spawn step waits for the transport endpoint to
    /// become reachable before marking the emulator as `Error`.
    /// Docker wallets need more headroom because the container has
    /// to pull / boot.
    pub startup_timeout_secs: u64,
    /// Binary names to look for under `~/.hwwtui/bundles/{wallet}/`
    /// when starting. First match wins. Lets the bundler ship
    /// either `trezor-emu-core` or `trezor-emu` depending on what
    /// the firmware build produced.
    pub binary_candidates: &'static [&'static str],
    /// Set to `false` for wallets that genuinely can't run on
    /// macOS even after the Linux-only UHID bits are gated off.
    /// BitBox02 and Coldcard host-side libs (`bitbox-api`,
    /// `coldcard` crates) expect HID transport — without UHID we
    /// can't simulate the USB layer for them on macOS.
    pub supports_macos: bool,
}

// ── HID report descriptors ────────────────────────────────────────────────────
//
// Static byte sequences the kernel uses to enumerate a virtual HID
// device. Identical to the constants currently in
// `crates/bridge/src/uhid.rs` — Phase 1c moves them out of there.
// Kept here so consumers don't have to import from two crates.

// Kept around in case a future test wants to stand up a virtual
// Trezor HID slot for the host-app side of the wire — today Trezor
// uses pure UDP so this is unused. `#[allow]` silences the dead-code
// warning until that test arrives.
#[allow(dead_code)]
const TREZOR_HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0xd0, 0xf1, 0x09, 0x01, 0xa1, 0x01, 0x09, 0x20, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08,
    0x95, 0x40, 0x81, 0x02, 0x09, 0x21, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91,
    0x02, 0xc0,
];

const BITBOX02_HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0xff, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40,
    0x09, 0x01, 0x81, 0x02, 0x09, 0x01, 0x91, 0x02, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00,
];

const COLDCARD_HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0xff, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40,
    0x09, 0x01, 0x81, 0x02, 0x09, 0x01, 0x91, 0x02, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00,
];

// ── Per-wallet configs ────────────────────────────────────────────────────────

pub const TREZOR: WalletConfig = WalletConfig {
    wallet_type: WalletType::Trezor,
    display_name: "Trezor",
    transport: TransportKind::Udp,
    default_port: 21324,
    host_mode: HostMode::Native,
    // Trezor doesn't need UHID — host apps speak the wire protocol
    // over UDP directly (trezor-client, python-trezor, sigvault-desktop
    // through the async-hwi trezor backend). The descriptor is still
    // kept around for tests that want to spin up a fake HID slot.
    hid: None,
    init_style: InitStyle::TrezorWireDebugLink,
    startup_timeout_secs: 10,
    binary_candidates: &["trezor-emu-core", "trezor-emu", "emulator"],
    // Trezor's unix port runs anywhere — no UHID dependency.
    supports_macos: true,
};

pub const BITBOX02: WalletConfig = WalletConfig {
    wallet_type: WalletType::BitBox02,
    display_name: "BitBox02",
    transport: TransportKind::Tcp,
    default_port: 15423,
    host_mode: HostMode::Native,
    hid: Some(HidConfig {
        vid: 0x03EB,
        pid: 0x2403,
        report_size: 64,
        report_descriptor: BITBOX02_HID_REPORT_DESCRIPTOR,
    }),
    init_style: InitStyle::BitBoxApi,
    startup_timeout_secs: 15,
    binary_candidates: &["bitbox02-simulator", "simulator"],
    // bitbox-api expects HID. No UHID on macOS ⇒ can't bridge.
    supports_macos: false,
};

pub const COLDCARD: WalletConfig = WalletConfig {
    wallet_type: WalletType::Coldcard,
    display_name: "Coldcard",
    // UnixDgram + fixed socket path — port is unused, kept at 0 for
    // the table shape but consumers must read the path from a
    // separate config (Phase 1c surfaces this properly).
    transport: TransportKind::UnixDgram,
    default_port: 0,
    host_mode: HostMode::Native,
    hid: Some(HidConfig {
        vid: 0xD13E,
        pid: 0xCC10,
        report_size: 64,
        report_descriptor: COLDCARD_HID_REPORT_DESCRIPTOR,
    }),
    init_style: InitStyle::Preseeded,
    startup_timeout_secs: 15,
    binary_candidates: &["coldcard-sim", "simulator.py"],
    // Same reason as BitBox02 — host-side `coldcard` crate uses HID.
    supports_macos: false,
};

pub const SPECTER: WalletConfig = WalletConfig {
    wallet_type: WalletType::Specter,
    display_name: "Specter DIY",
    transport: TransportKind::Tcp,
    default_port: 8789,
    host_mode: HostMode::Native,
    hid: None,
    init_style: InitStyle::Preseeded,
    startup_timeout_secs: 15,
    binary_candidates: &["specter-sim", "main.py"],
    supports_macos: true,
};

pub const LEDGER: WalletConfig = WalletConfig {
    wallet_type: WalletType::Ledger,
    display_name: "Ledger (Speculos)",
    transport: TransportKind::Tcp,
    default_port: 9999,
    host_mode: HostMode::Docker,
    hid: None,
    init_style: InitStyle::Preseeded,
    // Speculos pulls a Docker image on first run; give it room.
    startup_timeout_secs: 30,
    binary_candidates: &["speculos-runner.sh"],
    supports_macos: true,
};

pub const JADE: WalletConfig = WalletConfig {
    wallet_type: WalletType::Jade,
    display_name: "Blockstream Jade (QEMU)",
    transport: TransportKind::Tcp,
    default_port: 30121,
    host_mode: HostMode::Docker,
    hid: None,
    init_style: InitStyle::Preseeded,
    startup_timeout_secs: 30,
    binary_candidates: &["jade-runner.sh"],
    supports_macos: true,
};

/// All known wallet configs in the order the TUI presents them.
pub const ALL_WALLETS: &[&WalletConfig] =
    &[&TREZOR, &BITBOX02, &COLDCARD, &SPECTER, &LEDGER, &JADE];

/// Look up the config for a wallet variant. Total — every
/// `WalletType` has a matching entry, asserted by the
/// `every_wallet_type_has_a_config` test.
pub fn for_wallet(wallet: WalletType) -> &'static WalletConfig {
    match wallet {
        WalletType::Trezor => &TREZOR,
        WalletType::BitBox02 => &BITBOX02,
        WalletType::Coldcard => &COLDCARD,
        WalletType::Specter => &SPECTER,
        WalletType::Ledger => &LEDGER,
        WalletType::Jade => &JADE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wallet_type_has_a_config() {
        // Iterating `ALL_WALLETS` and checking each `wallet_type`
        // is exactly the inverse of `for_wallet`. If a new
        // `WalletType` variant is added but missed here, this test
        // is the first to catch it.
        let expected = [
            WalletType::Trezor,
            WalletType::BitBox02,
            WalletType::Coldcard,
            WalletType::Specter,
            WalletType::Ledger,
            WalletType::Jade,
        ];
        for wt in expected {
            let cfg = for_wallet(wt);
            assert_eq!(
                cfg.wallet_type, wt,
                "for_wallet({wt:?}) returned wrong config"
            );
        }
        assert_eq!(ALL_WALLETS.len(), expected.len());
    }

    #[test]
    fn hid_consumers_have_required_descriptor_fields() {
        for cfg in ALL_WALLETS.iter() {
            if let Some(hid) = cfg.hid {
                assert!(hid.vid != 0, "{} has zero VID", cfg.display_name);
                assert!(hid.pid != 0, "{} has zero PID", cfg.display_name);
                assert!(
                    hid.report_size > 0,
                    "{} has zero report_size",
                    cfg.display_name
                );
                assert!(
                    !hid.report_descriptor.is_empty(),
                    "{} has empty report_descriptor",
                    cfg.display_name
                );
            }
        }
    }

    #[test]
    fn supports_macos_implies_no_uhid_requirement() {
        // The macOS subset (Phase 2) skips UHID. If a wallet is
        // marked macOS-capable, it must NOT require UHID (i.e. it
        // must talk a network transport directly).
        for cfg in ALL_WALLETS.iter() {
            if cfg.supports_macos {
                assert!(
                    cfg.hid.is_none(),
                    "{} is marked macOS-capable but requires UHID — set supports_macos=false or rework the bridge",
                    cfg.display_name
                );
            }
        }
    }
}

//! Virtual HID device via Linux UHID (`/dev/uhid`).
//!
//! `VirtualHidDevice` wraps [`uhid_virt::UHIDDevice`] to create a virtual USB
//! HID device that appears in `/dev/hidraw*` and is indistinguishable from a
//! real plugged-in device from the perspective of userspace applications.
//!
//! # Blocking I/O
//!
//! The underlying `uhid-virt` crate opens `/dev/uhid` with `O_NONBLOCK`.
//! `read()` will return an `io::Error` with `WouldBlock` when no event is
//! queued. The bridge polls from a `spawn_blocking` thread to avoid burning
//! CPU.
//!
//! # Permissions
//!
//! Writing to `/dev/uhid` requires either `root` or membership in the
//! `uhid` / `plugdev` group, depending on your udev rules:
//!
//! ```text
//! KERNEL=="uhid", MODE="0660", GROUP="plugdev"
//! ```

use std::time::Duration;

use anyhow::Context;
use tracing::{debug, trace};
use uhid_virt::{Bus, CreateParams, OutputEvent, StreamError, UHIDDevice};

// ── VirtualHidDevice ──────────────────────────────────────────────────────────

/// A virtual HID device backed by `/dev/uhid`.
pub struct VirtualHidDevice {
    inner: UHIDDevice<std::fs::File>,
    name: String,
}

impl VirtualHidDevice {
    /// Create and register a new virtual HID device with the kernel.
    ///
    /// - `vid` / `pid`: USB vendor/product IDs presented to the OS.
    /// - `name`: Human-readable device name.
    /// - `report_descriptor`: Raw HID report descriptor bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if `/dev/uhid` cannot be opened (likely a permissions
    /// problem — see the module-level note on udev rules).
    pub fn new(vid: u16, pid: u16, name: &str, report_descriptor: &[u8]) -> anyhow::Result<Self> {
        let params = CreateParams {
            name: name.to_string(),
            phys: String::new(),
            uniq: String::new(),
            bus: Bus::USB,
            vendor: vid as u32,
            product: pid as u32,
            version: 0,
            country: 0,
            rd_data: report_descriptor.to_vec(),
        };

        let device = UHIDDevice::create(params).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create UHID device '{name}' (VID={vid:#06x} PID={pid:#06x}): {e}\n\
                 Hint: check that /dev/uhid is accessible. Try:\n\
                 \tsudo setfacl -m u:$USER:rw /dev/uhid\n\
                 or add a udev rule: KERNEL==\"uhid\", MODE=\"0660\", GROUP=\"plugdev\""
            )
        })?;

        debug!(name, vid, pid, "UHID device created");

        Ok(Self {
            inner: device,
            name: name.to_string(),
        })
    }

    /// Send an Input report from the (virtual) device to the host.
    ///
    /// The data should match the report size declared in the HID descriptor
    /// (64 bytes for Trezor). The kernel passes it up to the HID layer.
    pub fn send_input_report(&mut self, data: &[u8]) -> anyhow::Result<()> {
        trace!(name = %self.name, len = data.len(), "Sending input report to host");
        self.inner
            .write(data)
            .map(|_| ())
            .context("Failed to write UHID input report")
    }

    /// Poll for a single Output report from the host, returning immediately
    /// if none is ready.
    ///
    /// Returns `Ok(Some(data))` when a report arrives, `Ok(None)` on
    /// non-output events (Open/Close/Start/Stop) or when no event is queued,
    /// and an error only on hard I/O faults.
    ///
    /// Because `/dev/uhid` is opened with `O_NONBLOCK`, this never blocks.
    pub fn poll_output_report(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        match self.inner.read() {
            Ok(OutputEvent::Output { data }) => {
                trace!(name = %self.name, len = data.len(), "Received output report from host");
                Ok(Some(data))
            }
            Ok(OutputEvent::Open) => {
                debug!(name = %self.name, "Host opened HID device");
                Ok(None)
            }
            Ok(OutputEvent::Close) => {
                debug!(name = %self.name, "Host closed HID device");
                Ok(None)
            }
            Ok(OutputEvent::Start { dev_flags: _ }) => {
                debug!(name = %self.name, "UHID device started by kernel");
                Ok(None)
            }
            Ok(OutputEvent::Stop) => {
                debug!(name = %self.name, "UHID device stopped by kernel");
                Ok(None)
            }
            Ok(OutputEvent::GetReport { id, .. }) => {
                // Respond with an empty report to avoid the kernel hanging.
                self.inner.write_get_report_reply(id, 0, vec![]).ok();
                Ok(None)
            }
            Ok(OutputEvent::SetReport { id, .. }) => {
                self.inner.write_set_report_reply(id, 0).ok();
                Ok(None)
            }
            Err(StreamError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No event queued; that is fine with O_NONBLOCK.
                Ok(None)
            }
            Err(StreamError::Io(e)) => Err(anyhow::anyhow!("UHID read I/O error: {e}")),
            Err(StreamError::UnknownEventType(t)) => {
                // Future kernel events we do not know about; log and ignore.
                trace!(name = %self.name, event_type = t, "Unknown UHID event type (ignored)");
                Ok(None)
            }
        }
    }

    /// Spin-poll for an Output report, sleeping `interval` between polls.
    ///
    /// Returns `Ok(Some(data))` when a report arrives, `Ok(None)` after
    /// `timeout` has elapsed with no report, or an error on I/O failure.
    ///
    /// This is the blocking entry-point used by `spawn_blocking` threads.
    pub fn blocking_read_output(
        &mut self,
        timeout: Duration,
        interval: Duration,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(data) = self.poll_output_report()? {
                return Ok(Some(data));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(interval);
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

// ── Trezor HID descriptor ─────────────────────────────────────────────────────

/// HID report descriptor for Trezor T / Trezor One (FIDO transport).
///
/// Usage Page: FIDO Alliance (0xF1D0), Usage 1 (U2F/CTAP HID).
/// 64-byte Input and Output reports, no report ID.
pub const TREZOR_HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0xd0, 0xf1, // Usage Page (FIDO Alliance = 0xF1D0)
    0x09, 0x01, // Usage (1)
    0xa1, 0x01, // Collection (Application)
    0x09, 0x20, //   Usage (Input Report Data)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8 bits)
    0x95, 0x40, //   Report Count (64 bytes)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x09, 0x21, //   Usage (Output Report Data)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8 bits)
    0x95, 0x40, //   Report Count (64 bytes)
    0x91, 0x02, //   Output (Data, Variable, Absolute)
    0xc0, // End Collection
];

/// Trezor T USB Vendor ID.
pub const TREZOR_VID: u16 = 0x1209;
/// Trezor T USB Product ID.
pub const TREZOR_PID: u16 = 0x53C1;

// ── BitBox02 HID descriptor ──────────────────────────────────────────────────

/// BitBox02 USB Vendor ID.
pub const BITBOX02_VID: u16 = 0x03EB;
/// BitBox02 USB Product ID.
pub const BITBOX02_PID: u16 = 0x2403;

/// HID report descriptor for BitBox02.
///
/// Usage Page: Vendor-defined (0xFFFF), Usage 1.
/// 64-byte Input and Output reports, no report ID.
pub const BITBOX02_HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0xFF, 0xFF, // Usage Page (Vendor Defined = 0xFFFF)
    0x09, 0x01, // Usage (1)
    0xA1, 0x01, // Collection (Application)
    0x09, 0x20, //   Usage (Input Report Data)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8 bits)
    0x95, 0x40, //   Report Count (64 bytes)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x09, 0x21, //   Usage (Output Report Data)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8 bits)
    0x95, 0x40, //   Report Count (64 bytes)
    0x91, 0x02, //   Output (Data, Variable, Absolute)
    0xC0, // End Collection
];

// ── Coldcard HID descriptor ─────────────────────────────────────────────────

/// Coldcard USB Vendor ID.
pub const COLDCARD_VID: u16 = 0xD13E;
/// Coldcard USB Product ID.
pub const COLDCARD_PID: u16 = 0xCC10;

/// HID report descriptor for Coldcard.
///
/// Usage Page: FIDO Alliance (0xF1D0), Usage 1.
/// 64-byte Input and Output reports, no report ID.
pub const COLDCARD_HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0xD0, 0xF1, // Usage Page (FIDO Alliance = 0xF1D0)
    0x09, 0x01, // Usage (1)
    0xA1, 0x01, // Collection (Application)
    0x09, 0x20, //   Usage (Input Report Data)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8 bits)
    0x95, 0x40, //   Report Count (64 bytes)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x09, 0x21, //   Usage (Output Report Data)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8 bits)
    0x95, 0x40, //   Report Count (64 bytes)
    0x91, 0x02, //   Output (Data, Variable, Absolute)
    0xC0, // End Collection
];

// ── Ledger HID descriptor ───────────────────────────────────────────────────

/// Ledger USB Vendor ID.
pub const LEDGER_VID: u16 = 0x2C97;
/// Ledger USB Product ID (Nano S Plus / Nano X).
pub const LEDGER_PID: u16 = 0x1000;

/// HID report descriptor for Ledger.
///
/// Usage Page: Vendor-defined (0xFFA0), Usage 1.
/// 64-byte Input and Output reports, no report ID.
pub const LEDGER_HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0xA0, 0xFF, // Usage Page (Vendor Defined = 0xFFA0)
    0x09, 0x01, // Usage (1)
    0xA1, 0x01, // Collection (Application)
    0x09, 0x20, //   Usage (Input Report Data)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8 bits)
    0x95, 0x40, //   Report Count (64 bytes)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x09, 0x21, //   Usage (Output Report Data)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8 bits)
    0x95, 0x40, //   Report Count (64 bytes)
    0x91, 0x02, //   Output (Data, Variable, Absolute)
    0xC0, // End Collection
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_has_correct_length() {
        // The canonical FIDO descriptor used by Trezor is 34 bytes.
        assert_eq!(TREZOR_HID_REPORT_DESCRIPTOR.len(), 34);
    }

    #[test]
    fn descriptor_starts_with_fido_usage_page() {
        assert_eq!(
            &TREZOR_HID_REPORT_DESCRIPTOR[..3],
            &[0x06, 0xd0, 0xf1],
            "First three bytes must be the FIDO usage-page tag"
        );
    }

    #[test]
    fn descriptor_ends_with_end_collection() {
        assert_eq!(
            *TREZOR_HID_REPORT_DESCRIPTOR.last().unwrap(),
            0xc0,
            "Last byte must be End Collection"
        );
    }

    #[test]
    fn bitbox02_descriptor_valid() {
        assert_eq!(BITBOX02_HID_REPORT_DESCRIPTOR.len(), 34);
        assert_eq!(
            &BITBOX02_HID_REPORT_DESCRIPTOR[..3],
            &[0x06, 0xFF, 0xFF],
            "BitBox02 uses vendor usage page 0xFFFF"
        );
        assert_eq!(*BITBOX02_HID_REPORT_DESCRIPTOR.last().unwrap(), 0xC0);
    }

    #[test]
    fn coldcard_descriptor_valid() {
        assert_eq!(COLDCARD_HID_REPORT_DESCRIPTOR.len(), 34);
        assert_eq!(
            &COLDCARD_HID_REPORT_DESCRIPTOR[..3],
            &[0x06, 0xD0, 0xF1],
            "Coldcard uses FIDO usage page 0xF1D0"
        );
        assert_eq!(*COLDCARD_HID_REPORT_DESCRIPTOR.last().unwrap(), 0xC0);
    }

    #[test]
    fn ledger_descriptor_valid() {
        assert_eq!(LEDGER_HID_REPORT_DESCRIPTOR.len(), 34);
        assert_eq!(
            &LEDGER_HID_REPORT_DESCRIPTOR[..3],
            &[0x06, 0xA0, 0xFF],
            "Ledger uses vendor usage page 0xFFA0"
        );
        assert_eq!(*LEDGER_HID_REPORT_DESCRIPTOR.last().unwrap(), 0xC0);
    }
}

//! Trezor wire-protocol header decoder.
//!
//! ## Wire framing
//!
//! All HID reports are exactly 64 bytes. The first report of a message starts
//! with `?##` (`0x3f 0x23 0x23`), followed by:
//!
//! | Offset | Size | Description |
//! |--------|------|-------------|
//! | 3      | 2    | Message type ID (big-endian u16) |
//! | 5      | 4    | Total payload length (big-endian u32) |
//! | 9      | ≤55  | First chunk of protobuf payload |
//!
//! Continuation reports start with `?` (`0x3f`) only.
//!
//! This module decodes **headers only** — full protobuf parsing is out of scope
//! for Phase 1.

use crate::DecodedMessage;

// ── Message type registry ─────────────────────────────────────────────────────

/// Map a Trezor message type ID to its name.
///
/// Source: `common/protob/messages.proto` in trezor-firmware.
pub fn message_type_name(id: u16) -> &'static str {
    match id {
        0 => "Initialize",
        1 => "Ping",
        2 => "Success",
        3 => "Failure",
        4 => "ChangePin",
        5 => "WipeDevice",
        9 => "GetEntropy",
        10 => "Entropy",
        16 => "LoadDevice",
        17 => "ResetDevice",
        18 => "Features", // GetFeatures response (older numbering)
        19 => "GetFeatures",
        20 => "ButtonRequest",
        21 => "ButtonAck",
        22 => "SetBusy",
        23 => "ApplySettings",
        24 => "ApplyFlags",
        26 => "BackupDevice",
        27 => "EntropyRequest",
        28 => "EntropyAck",
        29 => "RecoveryDevice",
        30 => "WordRequest",
        31 => "WordAck",
        35 => "GetPublicKey",
        36 => "PublicKey",
        37 => "SignTx",
        38 => "TxRequest",
        39 => "TxAck",
        40 => "GetAddress",
        41 => "Address",
        42 => "SignMessage",
        43 => "VerifyMessage",
        44 => "MessageSignature",
        45 => "PassphraseRequest",
        46 => "PassphraseAck",
        50 => "Cancel",
        55 => "PinMatrixRequest",
        56 => "PinMatrixAck",
        100 => "CipherKeyValue",
        101 => "CipheredKeyValue",
        110 => "GetOwnershipId",
        111 => "OwnershipId",
        112 => "GetOwnershipProof",
        113 => "OwnershipProof",
        _ => "Unknown",
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("packet too short: need at least 9 bytes, got {0}")]
    TooShort(usize),
    #[error("invalid magic bytes: expected 3f 23 23, got {0:02x} {1:02x} {2:02x}")]
    BadMagic(u8, u8, u8),
    #[error("continuation packet passed to first-packet decoder")]
    ContinuationPacket,
}

// ── Decoder ───────────────────────────────────────────────────────────────────

/// Attempt to decode the header of a first HID report.
///
/// Returns a [`DecodedMessage`] with the message type name and payload length.
/// The protobuf payload itself is hex-dumped into `raw_hex` but not parsed.
///
/// # Errors
///
/// Returns [`DecodeError`] if the packet does not look like a valid Trezor
/// first-report.
pub fn decode_packet(data: &[u8]) -> Result<DecodedMessage, DecodeError> {
    if data.len() < 9 {
        return Err(DecodeError::TooShort(data.len()));
    }

    // First report: ?## (0x3f 0x23 0x23)
    if data[0] == 0x3f && data[1] != 0x23 {
        return Err(DecodeError::ContinuationPacket);
    }
    if data[0] != 0x3f || data[1] != 0x23 || data[2] != 0x23 {
        return Err(DecodeError::BadMagic(data[0], data[1], data[2]));
    }

    let msg_type_id = u16::from_be_bytes([data[3], data[4]]);
    let payload_len = u32::from_be_bytes([data[5], data[6], data[7], data[8]]);
    let type_name = message_type_name(msg_type_id);

    // Hex dump the entire raw report for the raw-messages panel.
    let raw_hex = data
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");

    Ok(DecodedMessage {
        wallet_type: "Trezor".to_string(),
        message_type: type_name.to_string(),
        fields: vec![
            ("type_id".to_string(), msg_type_id.to_string()),
            ("payload_len".to_string(), payload_len.to_string()),
        ],
        raw_hex,
    })
}

/// Returns `true` if `data` is a continuation report (starts with `0x3f` but
/// not followed by `0x23 0x23`).
pub fn is_continuation(data: &[u8]) -> bool {
    data.len() >= 3 && data[0] == 0x3f && !(data[1] == 0x23 && data[2] == 0x23)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_packet(msg_type: u16, payload_len: u32) -> Vec<u8> {
        let mut p = vec![0u8; 64];
        p[0] = 0x3f;
        p[1] = 0x23;
        p[2] = 0x23;
        let [t0, t1] = msg_type.to_be_bytes();
        p[3] = t0;
        p[4] = t1;
        let [l0, l1, l2, l3] = payload_len.to_be_bytes();
        p[5] = l0;
        p[6] = l1;
        p[7] = l2;
        p[8] = l3;
        p
    }

    #[test]
    fn decode_initialize() {
        let pkt = make_packet(0, 0);
        let msg = decode_packet(&pkt).unwrap();
        assert_eq!(msg.message_type, "Initialize");
    }

    #[test]
    fn decode_features() {
        let pkt = make_packet(17, 55);
        let msg = decode_packet(&pkt).unwrap();
        assert_eq!(msg.message_type, "ResetDevice");
    }

    #[test]
    fn too_short_errors() {
        assert!(matches!(
            decode_packet(&[0x3f, 0x23, 0x23]),
            Err(DecodeError::TooShort(_))
        ));
    }

    #[test]
    fn bad_magic_errors() {
        let pkt = vec![0u8; 64];
        assert!(matches!(
            decode_packet(&pkt),
            Err(DecodeError::BadMagic(..))
        ));
    }

    #[test]
    fn continuation_detection() {
        let mut pkt = vec![0u8; 64];
        pkt[0] = 0x3f;
        pkt[1] = 0x00;
        assert!(is_continuation(&pkt));
    }

    #[test]
    fn log_line_includes_fields() {
        let pkt = make_packet(0, 37);
        let msg = decode_packet(&pkt).unwrap();
        let line = msg.log_line();
        assert!(line.contains("Initialize"));
        assert!(line.contains("payload_len=37"));
    }
}

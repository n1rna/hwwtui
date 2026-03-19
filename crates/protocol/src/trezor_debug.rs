//! Trezor debug link client (UDP port 21325).
//!
//! The debug link runs on the Trezor emulator and exposes:
//! - Screen layout as structured JSON tokens via `DebugLinkGetState`
//! - Button/swipe/input injection via `DebugLinkDecision`
//!
//! ## Wire format
//!
//! Identical to the main Trezor port (64-byte framed UDP):
//! - First packet: `0x3F 0x23 0x23` + msg_type(u16 BE) + payload_len(u32 BE)
//!   + up to 55 bytes of payload, zero-padded to 64 bytes.
//! - Continuation packets: `0x3F` + up to 63 bytes of payload, zero-padded to 64 bytes.

use std::net::SocketAddr;

use anyhow::Context;
use tokio::net::UdpSocket;

// ── Message type IDs ──────────────────────────────────────────────────────────

const MSG_DEBUG_LINK_DECISION: u16 = 100;
const MSG_DEBUG_LINK_GET_STATE: u16 = 101;
const MSG_DEBUG_LINK_STATE: u16 = 102;

// ── Public enums ──────────────────────────────────────────────────────────────

/// Button choices available via the debug link.
#[derive(Debug, Clone, Copy)]
pub enum DebugButton {
    No = 0,
    Yes = 1,
    Info = 2,
}

/// Swipe directions available via the debug link.
#[derive(Debug, Clone, Copy)]
pub enum SwipeDirection {
    Up = 0,
    Down = 1,
    Left = 2,
    Right = 3,
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Async UDP client for the Trezor emulator debug link.
pub struct TrezorDebugLink {
    socket: UdpSocket,
    #[allow(dead_code)]
    addr: SocketAddr,
}

impl TrezorDebugLink {
    /// Connect a UDP socket to the debug link port (typically 21325).
    pub async fn connect(port: u16) -> anyhow::Result<Self> {
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
        // Bind to an OS-assigned local port.
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .context("Failed to bind debug-link UDP socket")?;
        socket
            .connect(addr)
            .await
            .context("Failed to connect debug-link UDP socket")?;
        Ok(Self { socket, addr })
    }

    // ── High-level API ────────────────────────────────────────────────────────

    /// Request the current screen layout and return the JSON token strings.
    ///
    /// The emulator returns a `DebugLinkState` message whose field 13 (`tokens`)
    /// contains the screen as a sequence of JSON strings that together form a
    /// complete JSON object when concatenated.
    pub async fn get_layout(&self) -> anyhow::Result<Vec<String>> {
        // DebugLinkGetState: field 3 (wait_layout) = 2 (CURRENT_LAYOUT)
        let payload = encode_field_varint(3, 2);
        self.send_message(MSG_DEBUG_LINK_GET_STATE, &payload)
            .await?;

        let response = self.recv_message_with_timeout().await?;

        // The response should be DebugLinkState (type 102).
        if response.msg_type != MSG_DEBUG_LINK_STATE {
            anyhow::bail!("Expected DebugLinkState (102), got {}", response.msg_type);
        }

        Ok(parse_tokens_field(&response.payload))
    }

    /// Send a button press to the emulator.
    pub async fn press_button(&self, button: DebugButton) -> anyhow::Result<()> {
        // DebugLinkDecision: field 1 (button) = varint
        let payload = encode_field_varint(1, button as u64);
        self.send_message(MSG_DEBUG_LINK_DECISION, &payload).await
    }

    /// Send a swipe gesture to the emulator.
    pub async fn swipe(&self, direction: SwipeDirection) -> anyhow::Result<()> {
        // DebugLinkDecision: field 2 (swipe) = varint
        let payload = encode_field_varint(2, direction as u64);
        self.send_message(MSG_DEBUG_LINK_DECISION, &payload).await
    }

    /// Send text input to the emulator.
    pub async fn input_text(&self, text: &str) -> anyhow::Result<()> {
        // DebugLinkDecision: field 3 (input) = string
        let payload = encode_field_string(3, text);
        self.send_message(MSG_DEBUG_LINK_DECISION, &payload).await
    }

    // ── Low-level framing ─────────────────────────────────────────────────────

    /// Frame `payload` into 64-byte UDP packets and send them.
    async fn send_message(&self, msg_type: u16, payload: &[u8]) -> anyhow::Result<()> {
        let packets = frame_message(msg_type, payload);
        for pkt in &packets {
            self.socket
                .send(pkt)
                .await
                .context("Debug-link send failed")?;
        }
        Ok(())
    }

    /// Receive framed UDP packets and reassemble into a single message,
    /// with a 500 ms timeout so we never block the TUI.
    async fn recv_message_with_timeout(&self) -> anyhow::Result<RawMessage> {
        let fut = self.recv_message();
        tokio::time::timeout(std::time::Duration::from_millis(500), fut)
            .await
            .context("Debug-link receive timed out")?
    }

    async fn recv_message(&self) -> anyhow::Result<RawMessage> {
        let mut buf = [0u8; 64];

        // First packet: must start with 0x3F 0x23 0x23.
        let n = self
            .socket
            .recv(&mut buf)
            .await
            .context("Debug-link recv failed")?;
        if n < 9 || buf[0] != 0x3F || buf[1] != 0x23 || buf[2] != 0x23 {
            anyhow::bail!("Debug-link: invalid first packet (n={n})");
        }

        let msg_type = u16::from_be_bytes([buf[3], buf[4]]);
        let payload_len = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;

        let mut payload = Vec::with_capacity(payload_len);
        let first_chunk = &buf[9..n.min(64)];
        payload.extend_from_slice(first_chunk);

        // Continuation packets.
        while payload.len() < payload_len {
            let n = self
                .socket
                .recv(&mut buf)
                .await
                .context("Debug-link recv continuation failed")?;
            if n < 1 || buf[0] != 0x3F {
                anyhow::bail!("Debug-link: invalid continuation packet");
            }
            // Continuation data starts at byte 1.
            let chunk = &buf[1..n.min(64)];
            payload.extend_from_slice(chunk);
        }

        payload.truncate(payload_len);
        Ok(RawMessage { msg_type, payload })
    }
}

// ── Main-port wire client ─────────────────────────────────────────────────

/// Simple async UDP client for the main Trezor wire port (21324).
/// Used to send commands that trigger UI flows on the emulator.
pub struct TrezorWireClient {
    socket: UdpSocket,
}

/// Well-known Trezor message types.
const MSG_INITIALIZE: u16 = 0;
const MSG_PING: u16 = 1;
const MSG_LOAD_DEVICE: u16 = 13;
const MSG_GET_FEATURES: u16 = 55;

impl TrezorWireClient {
    pub async fn connect(port: u16) -> anyhow::Result<Self> {
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .context("Failed to bind wire UDP socket")?;
        socket
            .connect(addr)
            .await
            .context("Failed to connect wire UDP socket")?;
        Ok(Self { socket })
    }

    /// Send Initialize (msg type 0, empty payload).
    /// Returns the Features response as raw protobuf bytes.
    pub async fn initialize(&self) -> anyhow::Result<Vec<u8>> {
        self.send_and_recv(MSG_INITIALIZE, &[]).await
    }

    /// Send Ping (msg type 1, empty payload).
    pub async fn ping(&self) -> anyhow::Result<Vec<u8>> {
        self.send_and_recv(MSG_PING, &[]).await
    }

    /// Send GetFeatures (msg type 55, empty payload).
    pub async fn get_features(&self) -> anyhow::Result<Vec<u8>> {
        self.send_and_recv(MSG_GET_FEATURES, &[]).await
    }

    /// Send LoadDevice with a test mnemonic (debug-only, msg type 13).
    /// This seeds the emulator so it behaves as an initialized device.
    ///
    /// LoadDevice protobuf:
    ///   field 1 (mnemonics, repeated string) — the seed phrase
    ///   field 3 (pin, string) — optional PIN
    ///   field 5 (label, string) — device label
    ///   field 7 (skip_checksum, bool) — skip mnemonic validation
    pub async fn load_device(&self, mnemonic: &str, label: &str) -> anyhow::Result<Vec<u8>> {
        let mut payload = Vec::new();
        // field 1: mnemonic (repeated string, but we send one)
        payload.extend(encode_field_string(1, mnemonic));
        // field 5: label
        if !label.is_empty() {
            payload.extend(encode_field_string(5, label));
        }
        // field 7: skip_checksum = false (validate the mnemonic)
        // (omitting it defaults to false, which is what we want)
        self.send_and_recv(MSG_LOAD_DEVICE, &payload).await
    }

    async fn send_and_recv(&self, msg_type: u16, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
        let packets = frame_message(msg_type, payload);
        for pkt in &packets {
            self.socket.send(pkt).await.context("Wire send failed")?;
        }

        // Read response with timeout.
        let mut buf = [0u8; 64];
        let recv_fut = async {
            let n = self.socket.recv(&mut buf).await?;
            if n < 9 || buf[0] != 0x3F || buf[1] != 0x23 || buf[2] != 0x23 {
                anyhow::bail!("Invalid wire response");
            }
            let resp_type = u16::from_be_bytes([buf[3], buf[4]]);
            let payload_len = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;
            let mut data = Vec::with_capacity(payload_len);
            data.extend_from_slice(&buf[9..n.min(64)]);
            while data.len() < payload_len {
                let n = self.socket.recv(&mut buf).await?;
                data.extend_from_slice(&buf[1..n.min(64)]);
            }
            data.truncate(payload_len);
            tracing::debug!(resp_type, payload_len, "Wire response received");
            Ok(data)
        };

        tokio::time::timeout(std::time::Duration::from_secs(2), recv_fut)
            .await
            .context("Wire response timed out")?
    }
}

// ── Internal types ────────────────────────────────────────────────────────────

struct RawMessage {
    msg_type: u16,
    payload: Vec<u8>,
}

// ── Wire framing helpers ──────────────────────────────────────────────────────

/// Split a protobuf payload into 64-byte framed UDP packets.
fn frame_message(msg_type: u16, payload: &[u8]) -> Vec<[u8; 64]> {
    let mut packets = Vec::new();

    // First packet header: 0x3F 0x23 0x23 + u16 msg_type BE + u32 len BE = 9 bytes header.
    let mut first = [0u8; 64];
    first[0] = 0x3F;
    first[1] = 0x23;
    first[2] = 0x23;
    let [t0, t1] = msg_type.to_be_bytes();
    first[3] = t0;
    first[4] = t1;
    let len_be = (payload.len() as u32).to_be_bytes();
    first[5] = len_be[0];
    first[6] = len_be[1];
    first[7] = len_be[2];
    first[8] = len_be[3];

    // Up to 55 bytes of payload in the first packet.
    let first_data = payload.len().min(55);
    first[9..9 + first_data].copy_from_slice(&payload[..first_data]);
    packets.push(first);

    // Remaining payload in 63-byte continuation chunks.
    let mut offset = first_data;
    while offset < payload.len() {
        let mut cont = [0u8; 64];
        cont[0] = 0x3F;
        let chunk_len = (payload.len() - offset).min(63);
        cont[1..1 + chunk_len].copy_from_slice(&payload[offset..offset + chunk_len]);
        packets.push(cont);
        offset += chunk_len;
    }

    packets
}

// ── Protobuf helpers ──────────────────────────────────────────────────────────

/// Encode a varint value into LEB-128 bytes.
fn encode_varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
    out
}

/// Decode a varint from a byte slice, returning `(value, bytes_consumed)`.
fn decode_varint(data: &[u8]) -> (u64, usize) {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    for (i, &byte) in data.iter().enumerate() {
        value |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return (value, i + 1);
        }
        shift += 7;
        if shift >= 64 {
            break;
        }
    }
    (value, data.len())
}

/// Encode a protobuf field with wire type 0 (varint).
fn encode_field_varint(field_num: u32, value: u64) -> Vec<u8> {
    // tag = (field_num << 3) | wire_type(0)
    let tag = (field_num as u64) << 3;
    let mut out = encode_varint(tag);
    out.extend(encode_varint(value));
    out
}

/// Encode a protobuf field with wire type 2 (length-delimited / string).
fn encode_field_string(field_num: u32, s: &str) -> Vec<u8> {
    // tag = (field_num << 3) | wire_type(2)
    let tag = ((field_num as u64) << 3) | 2;
    let bytes = s.as_bytes();
    let mut out = encode_varint(tag);
    out.extend(encode_varint(bytes.len() as u64));
    out.extend_from_slice(bytes);
    out
}

// ── DebugLinkState parser ─────────────────────────────────────────────────────

/// Extract all `tokens` strings (field 13, repeated string) from a
/// raw `DebugLinkState` protobuf payload.
fn parse_tokens_field(data: &[u8]) -> Vec<String> {
    // Field 13 tag: (13 << 3) | 2 = 106 = 0x6A
    const TOKENS_TAG: u64 = 0x6A;

    let mut tokens = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        // Decode field tag varint.
        let (tag, tag_len) = decode_varint(&data[pos..]);
        pos += tag_len;

        let wire_type = tag & 0x7;

        if tag == TOKENS_TAG {
            // Wire type 2: length-delimited.
            if pos >= data.len() {
                break;
            }
            let (str_len, len_bytes) = decode_varint(&data[pos..]);
            pos += len_bytes;
            let end = pos + str_len as usize;
            if end > data.len() {
                break;
            }
            if let Ok(s) = std::str::from_utf8(&data[pos..end]) {
                tokens.push(s.to_string());
            }
            pos = end;
        } else {
            // Skip unknown fields.
            match wire_type {
                0 => {
                    // Varint — consume it.
                    let (_, n) = decode_varint(&data[pos..]);
                    pos += n;
                }
                1 => {
                    // 64-bit fixed.
                    pos += 8;
                }
                2 => {
                    // Length-delimited.
                    if pos >= data.len() {
                        break;
                    }
                    let (len, n) = decode_varint(&data[pos..]);
                    pos += n + len as usize;
                }
                5 => {
                    // 32-bit fixed.
                    pos += 4;
                }
                _ => break, // Unknown wire type — stop parsing.
            }
        }
    }

    tokens
}

// ── Screen layout parser ──────────────────────────────────────────────────────

/// Parsed screen content extracted from debug-link layout tokens.
#[derive(Debug, Default, Clone)]
pub struct ParsedLayout {
    pub title: String,
    pub lines: Vec<String>,
    pub buttons: Vec<String>,
}

/// Parse a list of JSON token strings into a `ParsedLayout`.
///
/// The tokens, when concatenated, form a JSON object describing the screen.
/// We extract title, text content, and button labels.
pub fn parse_layout_tokens(tokens: &[String]) -> ParsedLayout {
    let json_str: String = tokens.concat();
    if json_str.is_empty() {
        return ParsedLayout::default();
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json_str) else {
        // Fall back: treat the raw tokens as plain text lines.
        return ParsedLayout {
            title: String::new(),
            lines: tokens.to_vec(),
            buttons: Vec::new(),
        };
    };

    let mut layout = ParsedLayout::default();
    extract_from_value(&value, &mut layout);
    layout
}

/// Recursively extract displayable text from a JSON value into `layout`.
fn extract_from_value(value: &serde_json::Value, layout: &mut ParsedLayout) {
    let Some(obj) = value.as_object() else {
        return;
    };

    let component = obj.get("component").and_then(|v| v.as_str()).unwrap_or("");

    // Extract title: try "title", then "label" (used by Homescreen), then "component" as fallback.
    if layout.title.is_empty() {
        if let Some(t) = obj.get("title").and_then(|v| v.as_str()) {
            if !t.is_empty() {
                layout.title = t.to_string();
            }
        }
    }
    if layout.title.is_empty() {
        if let Some(t) = obj.get("subtitle").and_then(|v| v.as_str()) {
            if !t.is_empty() {
                layout.title = t.to_string();
            }
        }
    }
    // For simple screens like Homescreen, use "label" as title.
    if layout.title.is_empty() {
        if let Some(t) = obj.get("label").and_then(|v| v.as_str()) {
            if !t.is_empty() {
                layout.title = t.to_string();
            }
        }
    }
    // Last resort: use the component name itself.
    if layout.title.is_empty() && !component.is_empty() {
        layout.title = component.to_string();
    }

    // Buttons object: {"left": {"text": "..."}, "right": {"text": "..."}}
    if let Some(buttons) = obj.get("buttons").and_then(|v| v.as_object()) {
        for btn_key in &["left", "middle", "right"] {
            if let Some(btn) = buttons.get(*btn_key) {
                // Button can be {"text": "..."} or just a string
                let txt = btn
                    .as_object()
                    .and_then(|b| b.get("text"))
                    .and_then(|v| v.as_str())
                    .or_else(|| btn.as_str());
                if let Some(t) = txt {
                    if !t.is_empty() {
                        layout.buttons.push(t.to_string());
                    }
                }
            }
        }
    }

    // Paragraphs array: [["line1"], ["line2"]] or [["word1", "word2"]]
    if let Some(paras) = obj.get("paragraphs").and_then(|v| v.as_array()) {
        for para in paras {
            let text = match para {
                serde_json::Value::Array(words) => words
                    .iter()
                    .filter_map(|w| w.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                serde_json::Value::String(s) => s.clone(),
                _ => continue,
            };
            if !text.is_empty() {
                layout.lines.push(text);
            }
        }
    }

    // "description" field (used by some confirmation screens)
    if let Some(desc) = obj.get("description").and_then(|v| v.as_str()) {
        if !desc.is_empty() {
            layout.lines.push(desc.to_string());
        }
    }

    // Plain "text" field
    if let Some(txt) = obj.get("text").and_then(|v| v.as_str()) {
        if !txt.is_empty() {
            layout.lines.push(txt.to_string());
        }
    }

    // "value" field (e.g. address display, amount)
    if let Some(val) = obj.get("value").and_then(|v| v.as_str()) {
        if !val.is_empty() {
            layout.lines.push(val.to_string());
        }
    }

    // "notification" field
    if let Some(notif) = obj.get("notification").and_then(|v| v.as_str()) {
        if !notif.is_empty() {
            layout.lines.push(format!("[{notif}]"));
        }
    }

    // Recurse into known sub-objects
    for key in &["content", "page", "inner"] {
        if let Some(sub) = obj.get(*key) {
            extract_from_value(sub, layout);
        }
    }

    // Recurse into "items" / "pages" arrays
    for key in &["items", "pages"] {
        if let Some(arr) = obj.get(*key).and_then(|v| v.as_array()) {
            for item in arr {
                extract_from_value(item, layout);
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for &v in &[0u64, 1, 127, 128, 300, 16383, 16384, u32::MAX as u64] {
            let encoded = encode_varint(v);
            let (decoded, _) = decode_varint(&encoded);
            assert_eq!(decoded, v, "varint roundtrip failed for {v}");
        }
    }

    #[test]
    fn encode_field_varint_tag() {
        // field 3, varint 2: tag = (3 << 3) | 0 = 24 = 0x18, value = 0x02
        let encoded = encode_field_varint(3, 2);
        assert_eq!(encoded, vec![0x18, 0x02]);
    }

    #[test]
    fn parse_tokens_empty() {
        let tokens: Vec<String> = vec![];
        let layout = parse_layout_tokens(&tokens);
        assert!(layout.title.is_empty());
        assert!(layout.lines.is_empty());
        assert!(layout.buttons.is_empty());
    }

    #[test]
    fn parse_tokens_confirm_tx() {
        let json = r#"{"component":"Frame","title":"Confirm Transaction","content":{"component":"FormattedText","paragraphs":[["Send 0.001 BTC"],["to bc1q..."]]},"buttons":{"left":{"text":"Cancel"},"right":{"text":"Confirm"}}}"#;
        let layout = parse_layout_tokens(&[json.to_string()]);
        assert_eq!(layout.title, "Confirm Transaction");
        assert!(layout.lines.iter().any(|l| l.contains("Send 0.001 BTC")));
        assert!(layout.buttons.contains(&"Cancel".to_string()));
        assert!(layout.buttons.contains(&"Confirm".to_string()));
    }

    #[test]
    fn framing_single_packet() {
        // A short payload fits in one first-packet.
        let payload = [0x18, 0x02];
        let packets = frame_message(MSG_DEBUG_LINK_GET_STATE, &payload);
        assert_eq!(packets.len(), 1);
        let pkt = &packets[0];
        assert_eq!(pkt[0], 0x3F);
        assert_eq!(pkt[1], 0x23);
        assert_eq!(pkt[2], 0x23);
        assert_eq!(
            u16::from_be_bytes([pkt[3], pkt[4]]),
            MSG_DEBUG_LINK_GET_STATE
        );
        assert_eq!(u32::from_be_bytes([pkt[5], pkt[6], pkt[7], pkt[8]]), 2);
        assert_eq!(pkt[9], 0x18);
        assert_eq!(pkt[10], 0x02);
    }

    #[test]
    fn framing_continuation() {
        // 60 bytes forces a continuation packet.
        let payload = vec![0xAAu8; 60];
        let packets = frame_message(MSG_DEBUG_LINK_GET_STATE, &payload);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[1][0], 0x3F);
        // First packet carries bytes 0..55, continuation carries 55..60.
        assert_eq!(packets[1][1], 0xAA);
    }

    #[test]
    fn parse_tokens_field_basic() {
        // Build a minimal DebugLinkState with one token string.
        // tag for field 13 = (13 << 3) | 2 = 0x6A
        let token = b"hello";
        let mut data = vec![0x6A];
        data.extend(encode_varint(token.len() as u64));
        data.extend_from_slice(token);
        let tokens = parse_tokens_field(&data);
        assert_eq!(tokens, vec!["hello"]);
    }
}

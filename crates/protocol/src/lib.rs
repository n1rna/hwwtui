//! Protocol codecs for hardware wallet wire formats.
//!
//! Each sub-module knows how to decode a wallet's framing layer into
//! human-readable [`DecodedMessage`] values for display in the TUI logs.

pub mod trezor;

// ── Shared types ──────────────────────────────────────────────────────────────

/// A decoded protocol message ready for display.
#[derive(Debug, Clone)]
pub struct DecodedMessage {
    /// E.g. "Trezor"
    pub wallet_type: String,
    /// E.g. "Initialize", "Features", "ButtonRequest"
    pub message_type: String,
    /// Key/value pairs extracted from the message, displayed in the log panel.
    pub fields: Vec<(String, String)>,
    /// Full raw bytes as a hex string.
    pub raw_hex: String,
}

impl DecodedMessage {
    /// Format for the method-call log panel (short single line).
    pub fn log_line(&self) -> String {
        if self.fields.is_empty() {
            self.message_type.clone()
        } else {
            let f = self
                .fields
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} ({})", self.message_type, f)
        }
    }
}

//! Wire types for Matrix AppService push transactions.

use serde::Deserialize;

// ── Matrix AppService push event types ────────────────────────────

/// The top-level push transaction payload.
#[derive(Deserialize)]
pub(super) struct Transaction {
    pub(super) events: Vec<PushEvent>,
}

/// A single event from the AppService push transaction.
#[derive(Deserialize)]
pub(crate) struct PushEvent {
    #[serde(rename = "type")]
    pub(super) event_type: String,
    pub(super) event_id: Option<String>,
    pub(super) room_id: Option<String>,
    pub(super) sender: Option<String>,
    pub(super) origin_server_ts: Option<i64>,
    pub(super) state_key: Option<String>,
    pub(super) content: Option<serde_json::Value>,
    /// The event this event redacts (for redaction events).
    pub(super) redacts: Option<String>,
    /// Whether the event has been redacted.
    #[allow(dead_code)]
    pub(super) unsigned: Option<UnsignedData>,
}

#[derive(Deserialize)]
pub(super) struct UnsignedData {
    // Ignored for now – may contain redacted_because etc.
}

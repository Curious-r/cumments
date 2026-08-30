//! Stable Matrix wire-format names used by Cumments.
//!
//! The namespace is the reverse-DNS form of `cumments.curious.host`, the
//! domain controlled by the project operator. Keep these names stable: once
//! events are written to Matrix, renaming requires a migration strategy.

/// Reverse-DNS namespace for all Cumments Matrix identifiers.
pub const MATRIX_NAMESPACE: &str = "host.curious.cumments";

/// State event type carrying a room's Cumments identity.
pub const ROOM_METADATA_EVENT_TYPE: &str = "host.curious.cumments.metadata";

/// Single content key under which all Cumments-specific `m.room.message`
/// fields live, so custom data stays in one clearly namespaced block.
pub const MESSAGE_CONTENT_KEY: &str = "host.curious.cumments.message";

/// Content key for the signed delete proof embedded in a redaction's
/// `reason`, kept separate from the message block so each event kind carries
/// its own schema under the Cumments namespace.
pub const REDACTION_PROOF_KEY: &str = "host.curious.cumments.redaction";

/// Body prefix of the token-DM message that proves ownership of a Matrix user
/// ID for a pending governance role.
pub const CLAIM_MESSAGE_PREFIX: &str = "cumments-claim:";

/// Current schema version for `host.curious.cumments.metadata` state events.
pub const METADATA_SCHEMA_VERSION: i64 = 1;

/// Current schema version for the `host.curious.cumments.message` content block.
pub const MESSAGE_SCHEMA_VERSION: i64 = 1;

/// Returns `true` for a supported metadata schema version.
/// `None` (absent field) is unsupported under the breaking v1 policy (legacy outside trusted boundary).
pub fn is_supported_metadata_schema(schema: Option<i64>) -> bool {
    matches!(schema, Some(1))
}

/// Returns `true` for a supported message-block schema version.
/// `None` (absent field) is unsupported under the breaking v1 policy.
pub fn is_supported_message_schema(schema: Option<i64>) -> bool {
    matches!(schema, Some(1))
}

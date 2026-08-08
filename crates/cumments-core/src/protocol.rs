//! Stable Matrix wire-format names used by Cumments.
//!
//! The namespace is the reverse-DNS form of `cumments.curious.host`, the
//! domain controlled by the project operator. Keep these names stable: once
//! events are written to Matrix, renaming requires a migration strategy.

/// Reverse-DNS namespace for all Cumments Matrix identifiers.
pub const MATRIX_NAMESPACE: &str = "host.curious.cumments";

/// State event type carrying a room's Cumments identity.
pub const METADATA_EVENT_TYPE: &str = "host.curious.cumments.metadata";

/// Single content key under which all Cumments-specific `m.room.message`
/// fields live, so custom data stays in one clearly namespaced block.
pub const MESSAGE_CONTENT_KEY: &str = MATRIX_NAMESPACE;

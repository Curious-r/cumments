//! Stable error classification for Matrix driver failures.
//!
//! The reconciler decides its retry/quarantine/retire policy from these
//! variants instead of matching error strings, so wording changes never
//! silently change failure handling.

/// Classification of Matrix driver failures that need a non-default reaction.
#[derive(Debug, thiserror::Error)]
pub enum MatrixError {
    /// The AS sender cannot govern the room (membership, power level or room
    /// type), so adoption is refused. The room should be quarantined.
    #[error("adoption refused for {room_id}: {reason}")]
    AdoptionRefused { room_id: String, reason: String },
    /// The room no longer exists or is inaccessible (e.g. tombstoned); the
    /// registry entry should be retired.
    #[error("room gone or inaccessible ({room_id}): {reason}")]
    RoomGone { room_id: String, reason: String },
}

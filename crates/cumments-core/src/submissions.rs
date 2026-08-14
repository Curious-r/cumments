//! Accepted asynchronous write units and their idempotency metadata.
//!
//! A command expresses what the user wants to do; once accepted by the API
//! it becomes a submission with a queue row id, status, retries and an
//! idempotency contract.

use crate::commands::{DeleteCommentCommand, PostCommentCommand, UpdateCommentCommand};

/// Idempotency metadata attached to one write request.
///
/// The key scopes retries to a single author, and the request fingerprint
/// detects reuse of the same key with a different request body.
#[derive(Clone, Debug)]
pub struct IdempotencyInput {
    pub author_public_key: String,
    pub key: String,
    pub request_fingerprint: String,
}

/// Result of an idempotency-aware submission save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdempotencyOutcome {
    /// A new submission was queued.
    Accepted { submission_id: i64 },
    /// The exact same request was already accepted; return the original id.
    Replayed { submission_id: i64 },
    /// The key is already bound to a different request fingerprint.
    Reused,
}

/// A post submission together with its queue row id.
#[derive(Debug, Clone)]
pub struct PendingPostSubmission {
    pub id: i64,
    pub command: PostCommentCommand,
}

/// A delete submission together with its queue row id.
#[derive(Debug, Clone)]
pub struct PendingDeleteSubmission {
    pub id: i64,
    pub command: DeleteCommentCommand,
}

/// An update submission together with its queue row id.
#[derive(Debug, Clone)]
pub struct PendingUpdateSubmission {
    pub id: i64,
    pub command: UpdateCommentCommand,
}

/// A post submission stuck in `waiting_for_sync`, with the recorded Matrix
/// event and room ids used to verify whether the event actually exists.
#[derive(Debug, Clone)]
pub struct StuckPostSubmission {
    pub id: i64,
    pub event_id: String,
    pub room_id: Option<String>,
}

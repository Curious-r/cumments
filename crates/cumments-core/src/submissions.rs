//! Accepted asynchronous write units and their idempotency metadata.
//!
//! A command expresses what the user wants to do; once accepted by the API
//! it becomes a submission with a queue row id, status, retries and an
//! idempotency contract.

use crate::commands::{DeleteCommentCommand, PostCommentCommand, UpdateCommentCommand};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Generate a fresh, namespaced transaction ID for a submission-driven
/// homeserver request.
///
/// All submission queues use UUID v4 transaction IDs, as recommended by the
/// Matrix spec: the homeserver must be able to tell a new request apart from
/// a retransmission of the same request. Some homeservers (notably tuwunel)
/// have been observed returning an event ID for deterministic
/// `cumments_<kind>_<id>` transactions without ever making the event
/// queryable. The ID is persisted on the submission row so retries reuse it;
/// only a confirmed-absent event clears it and allocates a new one.
pub fn fresh_transaction_id(kind: &str) -> String {
    format!("cumments_{}_{}", kind, Uuid::new_v4())
}

/// Derive a stable transaction ID for a synchronous Matrix write whose retry
/// payload is byte-for-byte identical.
///
/// Reactions and poll votes do not have a durable submission row. Their
/// semantic identity plus the signed PoW challenge acts as the attempt nonce:
/// an exact network retry reuses the same Matrix transaction ID, while a new
/// user action gets a fresh challenge and therefore a fresh transaction.
pub fn deterministic_transaction_id(kind: &str, identity_parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    for part in identity_parts {
        // Length-prefix each field so embedded separators cannot create
        // ambiguous identities.
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("cumments_{kind}_{}", hex::encode(hasher.finalize()))
}

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
    /// The transaction ID chosen for the latest send attempt, if any.
    /// `None` means the next attempt must allocate (and persist) a fresh one.
    pub txn_id: Option<String>,
}

/// A delete submission together with its queue row id.
#[derive(Debug, Clone)]
pub struct PendingDeleteSubmission {
    pub id: i64,
    pub command: DeleteCommentCommand,
    /// The transaction ID chosen for the latest send attempt, if any.
    pub txn_id: Option<String>,
}

/// An update submission together with its queue row id.
#[derive(Debug, Clone)]
pub struct PendingUpdateSubmission {
    pub id: i64,
    pub command: UpdateCommentCommand,
    /// The transaction ID chosen for the latest send attempt, if any.
    pub txn_id: Option<String>,
}

/// A post submission stuck in `waiting_for_sync`, with the recorded Matrix
/// event and room ids used to verify whether the event actually exists.
#[derive(Debug, Clone)]
pub struct StuckPostSubmission {
    pub id: i64,
    pub event_id: String,
    pub room_id: Option<String>,
}

/// A delete submission stuck in `waiting_for_sync`, with the recorded redaction
/// event and room ids used to verify whether the event actually exists.
#[derive(Debug, Clone)]
pub struct StuckDeleteSubmission {
    pub id: i64,
    pub event_id: String,
    pub room_id: Option<String>,
}

/// An update submission stuck in `waiting_for_sync`, with the recorded
/// replacement event and room ids used to verify whether it actually exists.
#[derive(Debug, Clone)]
pub struct StuckUpdateSubmission {
    pub id: i64,
    pub event_id: String,
    pub room_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{deterministic_transaction_id, fresh_transaction_id};
    use uuid::Uuid;

    #[test]
    fn fresh_transaction_ids_are_namespaced_and_unique() {
        let a = fresh_transaction_id("post");
        let b = fresh_transaction_id("post");
        assert!(a.starts_with("cumments_post_"));
        assert!(b.starts_with("cumments_post_"));
        assert_ne!(a, b);

        for txn in [a, b] {
            let tail = txn.strip_prefix("cumments_post_").expect("prefix");
            let uuid = Uuid::parse_str(tail).expect("v4 uuid tail");
            assert_eq!(uuid.get_version(), Some(uuid::Version::Random));
        }
    }

    #[test]
    fn deterministic_transaction_ids_are_stable_and_scope_sensitive() {
        let parts = ["site", "room", "$target", "like", "challenge|nonce"];
        let first = deterministic_transaction_id("react", &parts);
        let second = deterministic_transaction_id("react", &parts);
        assert_eq!(first, second);
        assert!(first.starts_with("cumments_react_"));

        let mut changed = parts;
        changed[3] = "award";
        assert_ne!(
            first,
            deterministic_transaction_id("react", &changed),
            "semantic payload must change the Matrix transaction"
        );
        assert_ne!(
            first,
            deterministic_transaction_id("vote", &parts),
            "action namespaces must not collide"
        );
    }

    #[test]
    fn deterministic_transaction_id_encoding_is_unambiguous() {
        assert_ne!(
            deterministic_transaction_id("react", &["a", "bc"]),
            deterministic_transaction_id("react", &["ab", "c"])
        );
    }
}

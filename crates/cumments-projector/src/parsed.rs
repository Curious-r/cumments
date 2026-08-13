//! Transport-agnostic parsed Matrix event structures.

use cumments_core::models::{Content, RoomIdentity};
use serde::Deserialize;

/// A parsed room message event.
#[derive(Debug)]
pub struct ParsedRoomMessage {
    pub room_id: String,
    pub event_id: String,
    /// The Matrix user ID of the sender.
    pub sender: String,
    /// The typed, displayable content of the message.
    pub content: Content,
    /// The resolved display name of the author, if available.
    pub display_name: Option<String>,
    /// The author's Ed25519 public key embedded in the event, if any.
    pub author_public_key: Option<String>,
    /// The author's Ed25519 signature embedded in the event, if any.
    pub author_signature: Option<String>,
    /// The PoW challenge prefix embedded in the event, if any.
    pub author_challenge: Option<String>,
    /// Whether the sender is one of our exclusive AS virtual users.
    pub is_virtual_user_sender: bool,
    /// Correlation hint: the submission queue row ID that produced this event,
    /// if the message was sent by Cumments.
    pub submission_id: Option<i64>,
    /// Matrix event ID of the parent comment, if this event is a rich reply.
    pub reply_to: Option<String>,
    /// Matrix event ID of the thread root, when this event belongs to a
    /// thread (m.thread).
    pub thread_root: Option<String>,
    pub origin_server_ts: i64,
    /// If this is an edit (m.replace), the relation details.
    pub relates_to: Option<ParsedRelation>,
    /// The room's Cumments identity, if it could be resolved.
    pub room_identity: Option<RoomIdentity>,
    /// The raw Matrix event content (forward-compatibility escape hatch).
    pub raw_content: serde_json::Value,
}

impl ParsedRoomMessage {
    /// The canonical value used to verify guest signatures: the text body for
    /// text messages, the MXC URL for media, and the geo URI for locations.
    pub fn signable_content(&self) -> Option<&str> {
        match &self.content {
            Content::Text(text) => Some(&text.body),
            Content::Media(media) => Some(&media.url),
            Content::Location(location) => Some(&location.geo_uri),
            _ => None,
        }
    }
}

/// A parsed relation (edit) attached to a message.
#[derive(Debug)]
pub struct ParsedRelation {
    pub target_event_id: String,
    pub new_content: Content,
}

impl ParsedRelation {
    /// The canonical value of the replacement (text edits today; media edits
    /// would sign the replacement media URL).
    pub fn signable_content(&self) -> Option<&str> {
        match &self.new_content {
            Content::Text(text) => Some(&text.body),
            Content::Media(media) => Some(&media.url),
            _ => None,
        }
    }
}

/// A parsed reaction event (m.reaction).
#[derive(Debug)]
pub struct ParsedReaction {
    pub room_id: String,
    pub event_id: String,
    pub sender: String,
    pub message_event_id: String,
    pub key: String,
    pub origin_server_ts: i64,
    /// Whether the sender is one of our exclusive AS virtual users.
    pub is_virtual_user_sender: bool,
    pub author_public_key: Option<String>,
    pub author_signature: Option<String>,
    pub author_challenge: Option<String>,
    pub room_identity: Option<RoomIdentity>,
}

/// A parsed poll response (msgtype `org.matrix.msc3381.poll.response`).
#[derive(Debug)]
pub struct ParsedPollVote {
    pub room_id: String,
    pub event_id: String,
    pub sender: String,
    pub poll_message_id: String,
    /// Answer IDs selected by the voter (typically one).
    pub answer_ids: Vec<String>,
    pub origin_server_ts: i64,
    pub is_virtual_user_sender: bool,
    pub author_public_key: Option<String>,
    pub author_signature: Option<String>,
    pub author_challenge: Option<String>,
    pub room_identity: Option<RoomIdentity>,
}

/// A parsed room state event (system message / room metadata).
#[derive(Debug)]
pub struct ParsedRoomState {
    pub room_id: String,
    pub event_id: String,
    pub sender: String,
    pub event_type: String,
    pub state_key: String,
    pub origin_server_ts: i64,
    pub content: serde_json::Value,
}

/// A parsed redaction event.
#[derive(Debug)]
pub struct ParsedRoomRedaction {
    pub room_id: String,
    pub event_id: String,
    /// Sender of the redaction event (e.g. the operator who deleted the
    /// message, or the AS sender for Cumments-issued deletions).
    pub sender: Option<String>,
    pub origin_server_ts: i64,
    /// The event ID being redacted (may be in `redacts` top-level or `.content.redacts`).
    pub redacts: Option<String>,
    /// The Cumments delete proof embedded in `reason`, if the redaction was
    /// issued through the Cumments API.
    pub proof: Option<serde_json::Value>,
    /// Correlation hint: the delete submission queue row ID embedded in the proof.
    pub submission_id: Option<i64>,
    /// The room's Cumments identity, if available.
    pub room_identity: Option<RoomIdentity>,
}

/// A parsed space-child state event (room added/removed from a Space).
#[derive(Debug)]
pub struct ParsedSpaceChild {
    pub space_room_id: String,
    /// The site_id resolved from the Space's own metadata.
    pub site_id: Option<String>,
    pub child_room_id: String,
    /// `true` if the child is being attached, `false` if removed.
    pub is_attached: bool,
    /// The child room's Cumments identity, if it could be resolved.
    pub child_room_identity: Option<RoomIdentity>,
}

// ── Metadata helpers ──────────────────────────────────────────────

/// Internal helper for deserialising Cumments room metadata.
#[derive(Deserialize)]
struct RoomMetadata {
    site_id: String,
    post_slug: Option<String>,
}

/// Resolve a `RoomIdentity` from optional metadata JSON and optional
/// canonical alias, using the same two-phase strategy as the original
/// `get_room_identity`:  (1) metadata state event, (2) alias fallback.
///
/// This is a **pure** function – no I/O, no SDK dependency.
pub fn parse_room_identity(
    metadata_json: Option<&str>,
    canonical_alias: Option<&str>,
) -> Option<RoomIdentity> {
    // Phase 1 – Try metadata first (source of truth)
    if let Some(json) = metadata_json
        && let Ok(m) = serde_json::from_str::<RoomMetadata>(json)
        && let Some(slug) = m.post_slug
    {
        return Some(RoomIdentity {
            site_id: m.site_id,
            post_slug: slug,
        });
    }

    // Phase 2 – Fallback to alias parsing for legacy rooms
    let alias = canonical_alias?;
    let alias_str = alias;

    // Supports #_cumments_SITE_ID_POST_SLUG:domain.
    let localpart = alias_str.split(':').next()?.strip_prefix('#')?;
    let content_part = localpart.strip_prefix("_cumments_")?;
    let parts: Vec<_> = content_part.splitn(2, '_').collect();

    if parts.len() == 2 {
        Some(RoomIdentity {
            site_id: parts[0].to_string(),
            post_slug: parts[1].to_string(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_room_identity_from_preferred_underscored_alias() {
        let identity =
            parse_room_identity(None, Some("#_cumments_my-blog_hello-world:example.com"));

        assert!(matches!(
            identity,
            Some(RoomIdentity {
                site_id,
                post_slug
            }) if site_id == "my-blog" && post_slug == "hello-world"
        ));
    }

    #[test]
    fn parse_room_identity_prefers_metadata_over_alias() {
        let metadata = r#"{"site_id": "meta-site", "post_slug": "meta-post"}"#;
        let identity = parse_room_identity(
            Some(metadata),
            Some("#_cumments_alias-site_alias-post:example.com"),
        );

        assert!(matches!(
            identity,
            Some(RoomIdentity {
                site_id,
                post_slug
            }) if site_id == "meta-site" && post_slug == "meta-post"
        ));
    }
}

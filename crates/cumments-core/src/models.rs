//! Defines the core data models of the application.
//! These models should be pure data structures with no logic tied to infrastructure.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use validator::Validate;

lazy_static::lazy_static! {
    /// Allowed chars: lowercase a-z, 0-9, hyphen.
    ///
    /// Uppercase and underscores are excluded deliberately: `site_id` and
    /// `post_slug` are embedded in Matrix user IDs and room aliases, where
    /// lowercase keeps user IDs spec-compliant and `_` stays a safe separator
    /// in `#_cumments_{site}_{post}` aliases.
    /// Length: 1–64 characters.
    pub static ref ID_REGEX: regex::Regex =
        regex::Regex::new(r"^[a-z0-9-]{1,64}$").unwrap();
}

// A validated, owned representation of a Site ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(transparent)]
pub struct SiteId {
    #[validate(regex(path = "*crate::models::ID_REGEX"))]
    pub id: String,
}

impl SiteId {
    /// Creates a new `SiteId` with validation.
    /// Returns `ValidationErrors` if the input doesn't match the expected format.
    pub fn new(id: String) -> Result<Self, validator::ValidationErrors> {
        let this = Self { id };
        this.validate()?;
        Ok(this)
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }
}

// Internal use only – data must already be validated.
// For untrusted input, use `SiteId::new()` which runs validation.
impl From<String> for SiteId {
    fn from(id: String) -> Self {
        Self { id }
    }
}

impl From<&str> for SiteId {
    fn from(s: &str) -> Self {
        Self { id: s.to_string() }
    }
}

// A validated, owned representation of a Post Slug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(transparent)]
pub struct PostSlug {
    #[validate(regex(path = "*crate::models::ID_REGEX"))]
    pub slug: String,
}

impl PostSlug {
    /// Creates a new `PostSlug` with validation.
    /// Returns `ValidationErrors` if the input doesn't match the expected format.
    pub fn new(slug: String) -> Result<Self, validator::ValidationErrors> {
        let this = Self { slug };
        this.validate()?;
        Ok(this)
    }

    pub fn as_str(&self) -> &str {
        &self.slug
    }
}

// Internal use only – data must already be validated.
// For untrusted input, use `PostSlug::new()` which runs validation.
impl From<String> for PostSlug {
    fn from(slug: String) -> Self {
        Self { slug }
    }
}

impl From<&str> for PostSlug {
    fn from(s: &str) -> Self {
        Self {
            slug: s.to_string(),
        }
    }
}

/// A message that has been projected into our read database.
/// This is the data structure that will be returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub event_id: String,
    pub site_id: String,
    pub post_slug: String,
    pub author: AuthorSnapshot,
    pub content: Content,
    pub timestamp: DateTime<Utc>,
    /// Matrix `origin_server_ts` of the last applied edit, converted to a
    /// timestamp. `None` when the message has never been edited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<DateTime<Utc>>,
    /// Matrix event ID of the parent message, when this message is a reply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Matrix event ID of the thread root, when this message belongs to a
    /// thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_root: Option<String>,
    /// Queue row ID of the intent that produced this message, when it was
    /// submitted through the Cumments API. Lets clients correlate a `202`
    /// response with the projected message/SSE event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<i64>,
    /// Lifecycle status of the message.
    pub status: MessageStatus,
    /// When the message was redacted; `None` while active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_at: Option<DateTime<Utc>>,
    /// Sender of the redaction event, when redacted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_by: Option<String>,
    /// Aggregated reaction counts for this message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<ReactionSummary>,
    /// Matrix room the message lives in. Internal integrity check for edits
    /// and redactions; never exposed through the API/SSE.
    #[serde(skip)]
    pub room_id: String,
    /// Matrix sender of the original event. Internal integrity check for
    /// edits (m.replace) and never exposed through the API/SSE.
    #[serde(skip)]
    pub sender_mxid: String,
    /// The raw Matrix event content, kept as an escape hatch for forward
    /// compatibility; never exposed through the API/SSE.
    #[serde(skip)]
    pub raw_content: serde_json::Value,
}

/// Lifecycle status of a message in the read model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Active,
    Redacted,
}

impl MessageStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageStatus::Active => "active",
            MessageStatus::Redacted => "redacted",
        }
    }
}

impl std::str::FromStr for MessageStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "active" => Ok(MessageStatus::Active),
            "redacted" => Ok(MessageStatus::Redacted),
            other => Err(format!("unknown message status `{other}`")),
        }
    }
}

/// Which identity model a message belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthorKind {
    /// Posted through the Cumments API by an AS virtual user; ownership is
    /// the Ed25519 public key embedded in the event.
    Guest,
    /// Posted directly in Matrix by a regular account; ownership is governed
    /// by Matrix (sender identity and room power levels).
    Matrix,
}

impl AuthorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthorKind::Guest => "guest",
            AuthorKind::Matrix => "matrix",
        }
    }
}

/// Author identity snapshot, captured when the message was projected.
///
/// - Guest messages carry `public_key`; `mxid` is intentionally not exposed
///   because the virtual user ID is an implementation detail derived from the
///   key and site.
/// - Matrix-native messages carry `mxid`; `public_key` is always `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorSnapshot {
    #[serde(rename = "type")]
    pub kind: AuthorKind,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub public_key: Option<String>,
    pub mxid: Option<String>,
}

/// The sealed set of displayable message contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text(TextContent),
    Media(MediaContent),
    Location(LocationContent),
    Poll(PollContent),
    Encrypted(EncryptedPlaceholder),
    Unknown(UnknownContent),
}

impl Content {
    pub fn kind(&self) -> &'static str {
        match self {
            Content::Text(_) => "text",
            Content::Media(_) => "media",
            Content::Location(_) => "location",
            Content::Poll(_) => "poll",
            Content::Encrypted(_) => "encrypted",
            Content::Unknown(_) => "unknown",
        }
    }
}

/// A plain-text message body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextContent {
    pub body: String,
    /// Formatted (HTML) body; the renderer MUST sanitize it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatted_body: Option<String>,
    pub style: TextStyle,
}

/// How a text message should be presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextStyle {
    Normal,
    Emote,
    Notice,
}

/// A media attachment (image/video/audio/file/sticker).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaContent {
    pub kind: MediaKind,
    /// MXC URI (`mxc://server/media_id`); downloads go through the media
    /// proxy.
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mimetype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_text: Option<String>,
    /// MSC3245 voice message marker.
    #[serde(default)]
    pub voice: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    File,
    Sticker,
}

/// A geo location (MSC3488).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationContent {
    pub geo_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
}

/// A poll (MSC3381). `responses` are hydrated from the poll-responses table
/// when reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollContent {
    pub question: String,
    pub options: Vec<PollOption>,
    #[serde(default)]
    pub responses: Vec<PollResponseSummary>,
}

/// One selectable poll option. The `id` matches Matrix's answer ID so votes
/// can be mapped back to an option index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollOption {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollResponseSummary {
    pub option_index: i64,
    pub count: i64,
}

/// An encrypted message placeholder; the content is never readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedPlaceholder {
    pub algorithm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_key: Option<String>,
}

/// An unknown/custom message type, degraded to a fallback body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Raw event content; also kept on `Message.raw_content`.
    pub raw: serde_json::Value,
}

/// One aggregated reaction on a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionSummary {
    pub key: String,
    pub count: i64,
}

/// A stored reaction event (one row per Matrix reaction event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub event_id: String,
    pub message_event_id: String,
    pub sender_mxid: String,
    pub key: String,
    pub origin_server_ts: i64,
    pub redacted_at: Option<DateTime<Utc>>,
}

/// One edit revision applied to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRevision {
    pub event_id: String,
    pub content: Content,
    pub edited_at: DateTime<Utc>,
    pub editor_mxid: String,
}

/// A poll vote record (one row per voter; the latest vote wins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollVote {
    /// Matrix event ID of the `m.poll.response` event, used to redact votes.
    pub event_id: String,
    pub poll_message_id: String,
    pub sender_mxid: String,
    pub option_index: i64,
    pub origin_server_ts: i64,
}

/// Media attached to a guest message (image/voice/file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentMedia {
    /// Explicit media kind; derived from `mimetype` when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MediaKind>,
    /// MXC URI of the uploaded media.
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mimetype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// MSC3245 voice message marker.
    #[serde(default)]
    pub voice: bool,
}

/// Represents a website that uses Cumments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub id: String,
    pub matrix_space_id: String,
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One page of room history fetched from the homeserver.
#[derive(Debug, Clone, Default)]
pub struct RoomEventPage {
    /// Raw Matrix room events (`m.room.message`, `m.room.redaction`, ...).
    pub events: Vec<serde_json::Value>,
    /// Token to continue fetching older history, if more is available.
    pub next_token: Option<String>,
    /// `true` when the homeserver reported more history in this direction.
    pub has_more: bool,
}

/// Identity of a Cumments room, extracted from metadata or alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomIdentity {
    pub site_id: String,
    pub post_slug: String,
}

/// Lifecycle state of a room in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoomStatus {
    /// The current canonical room for its site/post.
    Active,
    /// Adoption failed; the room is isolated and retried on a backoff
    /// schedule until it recovers or an operator reinstates it.
    Quarantined,
    /// Replaced by another room or no longer usable.
    Superseded,
}

impl RoomStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoomStatus::Active => "active",
            RoomStatus::Quarantined => "quarantined",
            RoomStatus::Superseded => "superseded",
        }
    }
}

impl std::str::FromStr for RoomStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "active" => Ok(RoomStatus::Active),
            "quarantined" => Ok(RoomStatus::Quarantined),
            "superseded" => Ok(RoomStatus::Superseded),
            other => Err(format!("unknown room status `{other}`")),
        }
    }
}

/// A room whose adoption failed and is currently quarantined, for operator
/// visibility and manual recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantinedRoom {
    pub room_id: String,
    pub site_id: String,
    pub post_slug: String,
    pub quarantine_reason: String,
    pub quarantined_at: DateTime<Utc>,
    pub adoption_failures: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

/// One page of projected messages for a site/post.
#[derive(Debug, Clone, Default)]
pub struct MessagePage {
    pub items: Vec<Message>,
    pub total: i64,
}

/// A room member profile snapshot (from `m.room.member` state events).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMember {
    pub room_id: String,
    pub user_id: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    /// Matrix membership: `join`, `invite`, `leave`, `ban`.
    pub membership: String,
    pub updated_at: DateTime<Utc>,
}

/// A raw room state event kept for the system-message feed and room metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomStateEvent {
    pub event_id: String,
    pub room_id: String,
    pub event_type: String,
    pub state_key: String,
    pub sender: String,
    pub origin_server_ts: i64,
    pub content_json: serde_json::Value,
}

/// Current room metadata derived from the latest state events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMetadata {
    pub room_id: String,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub avatar_url: Option<String>,
    /// Number of members with `join` membership.
    pub member_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_id_and_post_slug_accept_lowercase_hyphenated_slugs() {
        assert!(SiteId::new("my-blog".to_string()).is_ok());
        assert!(PostSlug::new("hello-world".to_string()).is_ok());
        assert!(SiteId::new("a1-b2".to_string()).is_ok());
    }

    #[test]
    fn site_id_and_post_slug_reject_underscores_and_uppercase() {
        assert!(SiteId::new("my_blog".to_string()).is_err());
        assert!(PostSlug::new("hello_world".to_string()).is_err());
        assert!(SiteId::new("My-Blog".to_string()).is_err());
        assert!(PostSlug::new("Hello-World".to_string()).is_err());
    }

    #[test]
    fn message_status_round_trips_through_db_values() {
        assert_eq!(MessageStatus::Active.as_str(), "active");
        assert_eq!(MessageStatus::Redacted.as_str(), "redacted");
        assert_eq!("active".parse::<MessageStatus>(), Ok(MessageStatus::Active));
        assert_eq!(
            "redacted".parse::<MessageStatus>(),
            Ok(MessageStatus::Redacted)
        );
        assert!("bogus".parse::<MessageStatus>().is_err());
    }

    #[test]
    fn content_kind_names_are_stable_and_lowercase() {
        let content = Content::Text(TextContent {
            body: "hi".to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        });
        assert_eq!(content.kind(), "text");
    }
}

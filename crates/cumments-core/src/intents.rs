//! Defines the "Intents" of the system.
//! An intent is a serializable struct that represents a user's desire
//! for the system to perform an action. It's the primary object written
//! by the API layer and consumed by the Reconciler layer.

use crate::models::{CommentMedia, PostSlug, SiteId};
use serde::{Deserialize, Serialize};

/// Represents the user's desire to post a comment.
/// This is a command to be processed asynchronously by the reconciler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostCommentIntent {
    /// The site this comment belongs to.
    pub site_id: SiteId,
    /// The post/page this comment belongs to.
    pub post_slug: PostSlug,

    /// The content of the comment, likely in Markdown.
    pub content: String,
    /// Optional media attachment (image/voice/file); when present, `content`
    /// carries the fallback filename and the signature covers the media URL.
    #[serde(default)]
    pub media: Option<CommentMedia>,

    /// Display name of the author. For guests, this is provided by them.
    pub display_name: String,

    /// Ed25519 public key identifying the author (base64url).
    /// Ownership is publicly verifiable from Matrix events.
    pub author_public_key: String,
    /// Ed25519 signature over the canonical request message.
    pub author_signature: String,
    /// PoW challenge prefix included in the signed message. Published in the
    /// Matrix event so the signature remains independently verifiable.
    #[serde(default)]
    pub author_challenge: String,

    /// If this comment is a reply, this field holds the ID of the parent comment.
    pub reply_to: Option<String>,
}

/// Represents the user's desire to delete a comment.
/// This is a command to be processed asynchronously by the reconciler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteCommentIntent {
    /// The site this comment belongs to.
    pub site_id: SiteId,
    /// The post/page this comment belongs to.
    pub post_slug: PostSlug,
    /// The Matrix event ID of the comment to be deleted.
    pub event_id: String,
    /// The author's Ed25519 public key (base64url).
    pub author_public_key: String,
    /// Ed25519 signature authorizing this deletion.
    pub author_signature: String,
    /// PoW challenge prefix included in the signed message. Published in the
    /// redaction reason so the signature remains independently verifiable
    /// from the event log.
    #[serde(default)]
    pub author_challenge: String,
}

/// Represents the user's desire to edit/update a comment.
/// This is a command to be processed asynchronously by the reconciler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCommentIntent {
    /// The site this comment belongs to.
    pub site_id: SiteId,
    /// The post/page this comment belongs to.
    pub post_slug: PostSlug,
    /// The Matrix event ID of the comment to be updated.
    pub event_id: String,
    /// The new content for the comment.
    pub content: String,
    /// The author's Ed25519 public key (base64url).
    pub author_public_key: String,
    /// Ed25519 signature authorizing this edit.
    pub author_signature: String,
    /// PoW challenge prefix included in the signed message. Published in the
    /// replacement event so the signature remains independently verifiable.
    #[serde(default)]
    pub author_challenge: String,
}

/// A post intent together with its queue row id.
#[derive(Debug, Clone)]
pub struct PendingPostIntent {
    pub id: i64,
    pub intent: PostCommentIntent,
}

/// A delete intent together with its queue row id.
#[derive(Debug, Clone)]
pub struct PendingDeleteIntent {
    pub id: i64,
    pub intent: DeleteCommentIntent,
}

/// An update intent together with its queue row id.
#[derive(Debug, Clone)]
pub struct PendingUpdateIntent {
    pub id: i64,
    pub intent: UpdateCommentIntent,
}

/// A post intent stuck in `waiting_for_sync`, with the recorded Matrix event
/// and room ids used to verify whether the event actually exists.
#[derive(Debug, Clone)]
pub struct StuckPostIntent {
    pub id: i64,
    pub event_id: String,
    pub room_id: Option<String>,
}

//! Defines the "Intents" of the system.
//! An intent is a serializable struct that represents a user's desire
//! for the system to perform an action. It's the primary object written
//! by the API layer and consumed by the Reconciler layer.

use crate::models::{PostSlug, SiteId};
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

    /// Information about the author. For guests, this is provided by them.
    pub nickname: String,
    // Optional, maybe for gravatar-like features later. PII retention note:
    // the email is stored only in this intent payload inside the local SQLite
    // queue and never leaves it (not written to Matrix events or the read
    // model). Completed rows are kept as an audit log; scrub or add retention
    // cleanup before production deployment.
    pub email: Option<String>,

    /// Ed25519 public key identifying the author (base64url).
    /// Ownership is publicly verifiable from Matrix events.
    pub author_public_key: String,
    /// Ed25519 signature over the canonical request message.
    pub author_signature: String,

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
}

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
    pub email: Option<String>, // Optional, maybe for gravatar-like features later

    /// A stable fingerprint identifying the guest user, derived from the `guest_token`.
    pub author_fingerprint: String,

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
    /// The fingerprint of the user attempting to delete the comment, for verification.
    pub author_fingerprint: String,
}

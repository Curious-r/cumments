use anyhow::Result;
use async_trait::async_trait;
use cumments_core::intents::{DeleteCommentIntent, PostCommentIntent};

pub mod bot;
pub mod logging;

/// Defines the actions that can be performed on the Matrix network.
/// This trait is the boundary between the reconciler (the "brain") and the
/// operator (the "hands").
#[async_trait]
pub trait MatrixOperator: Send + Sync {
    /// Posts a comment to the appropriate Matrix room.
    ///
    /// This method is responsible for:
    /// 1. Finding or creating the correct Matrix room for the given `site_id` and `post_slug`.
    /// 2. Formatting the comment content appropriately.
    /// 3. Sending the message to the room.
    ///
    /// # Returns
    /// The event ID of the newly posted message.
    async fn post_comment(&self, intent: &PostCommentIntent) -> Result<String>;

    /// Redacts a comment in the appropriate Matrix room.
    async fn redact_comment(&self, intent: &DeleteCommentIntent) -> Result<()>;
}

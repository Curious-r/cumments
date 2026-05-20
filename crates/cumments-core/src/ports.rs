use crate::intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent};
use crate::models::{Comment, PostSlug, SiteId};
use anyhow::Result;
use async_trait::async_trait;

/// The port for all intent storage operations.
#[async_trait]
pub trait IntentStore: Send + Sync {
    async fn save_post_intent(&self, intent: &PostCommentIntent) -> Result<()>;
    async fn save_delete_intent(&self, intent: &DeleteCommentIntent) -> Result<()>;
    async fn save_update_intent(&self, intent: &UpdateCommentIntent) -> Result<()>;

    /// Transitions a post intent to 'waiting_for_sync' and records the Matrix event ID.
    async fn mark_post_intent_waiting_for_sync(&self, id: i64, event_id: &str) -> Result<()>;

    /// Transitions an update intent to 'waiting_for_sync'.
    async fn mark_update_intent_waiting_for_sync(&self, id: i64) -> Result<()>;

    /// Transitions a post intent to 'completed' when the projector sees the event.
    async fn mark_post_intent_completed(&self, event_id: &str) -> Result<()>;

    /// Transitions a delete intent to 'completed' when the projector sees the redaction.
    async fn mark_delete_intent_completed(&self, target_event_id: &str) -> Result<()>;

    /// Transitions an update intent to 'completed' when the projector sees the replacement.
    async fn mark_update_intent_completed(&self, event_id: &str) -> Result<()>;
}

/// The port for all comment projection storage operations.
#[async_trait]
pub trait CommentStore: Send + Sync {
    /// Fetches a single comment by its Matrix event ID.
    async fn get_comment(&self, event_id: &str) -> Result<Option<Comment>>;

    /// Fetches a paginated list of projected comments for a given site and post.
    /// Returns the list of comments and the total number of comments.
    async fn get_comments(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Comment>, i64)>;
}

/// Port for managing the local room registry cache (Mirror of Space relationships).
#[async_trait]
pub trait RegistryStore: Send + Sync {
    /// Returns the room ID for a site/post from the local registry, if it exists and is active.
    async fn get_registered_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<Option<String>>;

    /// Invalidates a room in the local registry (e.g. if metadata verification failed).
    async fn invalidate_room_registry(&self, room_id: &str) -> Result<()>;
}

/// Defines the operations for managing sites in the local database.
#[async_trait]
pub trait SiteStore: Send + Sync {
    async fn get_site(&self, id: &SiteId) -> Result<Option<crate::models::Site>>;
    async fn save_site(&self, site: &crate::models::Site) -> Result<()>;
}

/// Defines the atomic actions that can be performed on the Matrix network.
/// This is the "Hands" of the system.
#[async_trait]
pub trait MatrixDriver: Send + Sync {
    /// Ensures a room exists for a specific post and is linked to a space.
    /// Uses candidate_room_id as a hint for O(1) discovery if provided.
    /// Returns the room ID.
    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        space_id: &str,
        candidate_room_id: Option<&str>,
    ) -> Result<String>;

    /// Creates a new Space for a site.
    /// Returns the new Space's room ID.
    async fn create_site_space(&self, site_id: &SiteId) -> Result<String>;

    /// Posts a message to a specific room.
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        nickname: &str,
        fingerprint: &str,
    ) -> Result<String>;

    /// Updates an existing message in a specific room using m.replace.
    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        nickname: &str,
        fingerprint: &str,
    ) -> Result<String>;

    /// Redacts a message in a specific room.
    async fn redact_message(&self, room_id: &str, event_id: &str) -> Result<()>;
}

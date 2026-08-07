use crate::intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent};
use crate::models::{Comment, PostSlug, SiteId};
use anyhow::Result;
use async_trait::async_trait;

/// The port for all intent storage operations.
#[async_trait]
pub trait IntentStore: Send + Sync {
    async fn save_post_intent(
        &self,
        intent: &PostCommentIntent,
        author_token_hash: Option<&str>,
    ) -> Result<()>;
    async fn save_delete_intent(&self, intent: &DeleteCommentIntent) -> Result<()>;
    async fn save_update_intent(&self, intent: &UpdateCommentIntent) -> Result<()>;

    async fn get_pending_post_intents(&self) -> Result<Vec<(i64, PostCommentIntent)>>;
    async fn get_pending_delete_intents(&self) -> Result<Vec<(i64, DeleteCommentIntent)>>;
    async fn get_pending_update_intents(&self) -> Result<Vec<(i64, UpdateCommentIntent)>>;

    /// Transitions a post intent to 'waiting_for_sync' and records the Matrix event ID.
    async fn mark_post_intent_waiting_for_sync(&self, id: i64, event_id: &str) -> Result<()>;

    /// Completes a post intent by its queue ID (used when the projector sees
    /// the event before the reconciler has written back the Matrix event ID).
    async fn mark_post_intent_completed_by_id(&self, id: i64) -> Result<()>;

    /// Transitions an update intent to 'waiting_for_sync'.
    async fn mark_update_intent_waiting_for_sync(&self, id: i64) -> Result<()>;

    /// Transitions a delete intent to 'waiting_for_sync'.
    async fn mark_delete_intent_waiting_for_sync(&self, id: i64) -> Result<()>;

    /// Transitions an intent to 'failed' status.
    async fn mark_post_intent_failed(&self, id: i64) -> Result<()>;
    async fn mark_delete_intent_failed(&self, id: i64) -> Result<()>;
    async fn mark_update_intent_failed(&self, id: i64) -> Result<()>;

    /// Transitions a post intent to 'completed' when the projector sees the event.
    async fn mark_post_intent_completed(&self, event_id: &str) -> Result<()>;

    /// Returns the stored author token hash for a post intent by its queue ID.
    async fn get_post_intent_token_hash_by_id(&self, id: i64) -> Result<Option<String>>;

    /// Returns the stored author token hash for a post intent, if any,
    /// looked up by the Matrix event ID recorded at send time.
    async fn get_post_intent_token_hash_by_event_id(
        &self,
        event_id: &str,
    ) -> Result<Option<String>>;

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

    /// Saves a new comment or updates an existing one (on conflict).
    async fn save_comment(
        &self,
        comment: &Comment,
        room_id: &str,
        site_id: &SiteId,
        post_slug: &PostSlug,
        author_token_hash: Option<&str>,
    ) -> Result<()>;

    /// Updates only the content of a comment.
    async fn update_comment_content(&self, event_id: &str, content: &str) -> Result<bool>;

    /// Deletes a comment by its event ID.
    async fn delete_comment(&self, event_id: &str) -> Result<bool>;

    /// Gets the author nickname for a specific event.
    async fn get_author_nickname(&self, event_id: &str) -> Result<Option<String>>;

    /// Returns the stored owner verifier (salt-keyed token hash) for a comment,
    /// if any. Used to authorize edit/delete requests without exposing the
    /// raw visitor token.
    async fn get_comment_author_token_hash(&self, event_id: &str) -> Result<Option<String>>;
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

    /// Checks if a room is active in the registry.
    async fn is_room_active(&self, room_id: &str) -> Result<Option<bool>>;

    /// Looks up the Cumments identity (`(site_id, post_slug)`) registered for a room.
    ///
    /// Unlike [`Self::get_registered_room`] this is a reverse lookup by room ID,
    /// used by the projector to resolve the context of incoming Matrix events
    /// without depending on room state metadata.
    async fn get_registered_room_identity(&self, room_id: &str)
    -> Result<Option<(String, String)>>;

    /// Registers or reactivates a room in the registry.
    async fn register_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<()>;

    /// Invalidates a room in the local registry (e.g. if metadata verification failed).
    async fn invalidate_room_registry(&self, room_id: &str) -> Result<()>;
}

/// Defines the operations for managing sites in the local database.
#[async_trait]
pub trait SiteStore: Send + Sync {
    async fn get_site(&self, id: &SiteId) -> Result<Option<crate::models::Site>>;

    /// Looks up a site by its Matrix Space room ID.
    async fn get_site_by_space_id(&self, space_id: &str) -> Result<Option<crate::models::Site>>;

    async fn save_site(&self, site: &crate::models::Site) -> Result<()>;

    /// Ensures a site exists in the database, creating it with default values if not.
    async fn ensure_site_exists(&self, site_id: &str, matrix_space_id: &str) -> Result<()>;
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
        site_id: &SiteId,
        // Correlation hint: the intent queue row ID, published in the event so
        // the projector can close the loop even if the push arrives before the
        // reconciler's write-back.
        intent_id: Option<i64>,
    ) -> Result<String>;

    /// Updates an existing message in a specific room using m.replace.
    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        nickname: &str,
        fingerprint: &str,
        site_id: &SiteId,
    ) -> Result<String>;

    /// Redacts a message in a specific room.
    async fn redact_message(&self, room_id: &str, event_id: &str) -> Result<()>;
}

/// Port for virtual user identity management (AppService mode).
/// Maps Cumments visitor fingerprints to stable Matrix virtual user IDs.
#[async_trait]
pub trait VirtualUserStore: Send + Sync {
    /// Returns the virtual Matrix user ID for the given fingerprint and site.
    /// Creates one deterministically if it doesn't exist yet.
    ///
    /// Format: `@_cumments_{site_id}_{sha256_trunc8(fingerprint)}:{server_name}`
    async fn get_or_create_virtual_user(
        &self,
        fingerprint: &str,
        site_id: &SiteId,
        server_name: &str,
    ) -> Result<String>;
}

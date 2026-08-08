use crate::intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent};
use crate::models::{Comment, PostSlug, RoomEventPage, SiteId};
use anyhow::Result;
use async_trait::async_trait;

/// The port for all intent storage operations.
#[async_trait]
pub trait IntentStore: Send + Sync {
    async fn save_post_intent(&self, intent: &PostCommentIntent) -> Result<()>;
    async fn save_delete_intent(&self, intent: &DeleteCommentIntent) -> Result<()>;
    async fn save_update_intent(&self, intent: &UpdateCommentIntent) -> Result<()>;

    async fn get_pending_post_intents(&self) -> Result<Vec<(i64, PostCommentIntent)>>;
    async fn get_pending_delete_intents(&self) -> Result<Vec<(i64, DeleteCommentIntent)>>;
    async fn get_pending_update_intents(&self) -> Result<Vec<(i64, UpdateCommentIntent)>>;

    /// Transitions a post intent to 'waiting_for_sync' and records the Matrix event ID.
    async fn mark_post_intent_waiting_for_sync(
        &self,
        id: i64,
        event_id: &str,
        room_id: &str,
    ) -> Result<()>;

    /// Completes a post intent by its queue ID (used when the projector sees
    /// the event before the reconciler has written back the Matrix event ID).
    async fn mark_post_intent_completed_by_id(&self, id: i64) -> Result<()>;

    /// Transitions an update intent to 'waiting_for_sync'.
    async fn mark_update_intent_waiting_for_sync(&self, id: i64, room_id: &str) -> Result<()>;

    /// Completes a specific update intent by its queue ID. Used when the edit
    /// event carries `host.curious.cumments.intent_id`, so completing one edit
    /// never closes a different queued edit targeting the same original
    /// comment.
    async fn mark_update_intent_completed_by_id(&self, id: i64) -> Result<()>;

    /// Transitions a delete intent to 'waiting_for_sync'.
    async fn mark_delete_intent_waiting_for_sync(&self, id: i64, room_id: &str) -> Result<()>;

    /// Records a processing failure. Returns `true` if the intent was
    /// scheduled for another attempt (pending + backoff), `false` if the
    /// retry budget is exhausted and the intent moves to 'failed'.
    async fn record_post_intent_failure(&self, id: i64, error: &str) -> Result<bool>;
    async fn record_delete_intent_failure(&self, id: i64, error: &str) -> Result<bool>;
    async fn record_update_intent_failure(&self, id: i64, error: &str) -> Result<bool>;

    /// Transitions a post intent to 'completed' when the projector sees the event.
    async fn mark_post_intent_completed(&self, event_id: &str) -> Result<()>;

    /// Post intents stuck in `waiting_for_sync` since before `cutoff`,
    /// as `(id, matrix_event_id, room_id)`.
    async fn get_stuck_post_intents(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<(i64, String, Option<String>)>>;

    /// IDs of delete intents stuck in `waiting_for_sync` since before `cutoff`.
    async fn get_stuck_delete_intent_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<i64>>;

    /// IDs of update intents stuck in `waiting_for_sync` since before `cutoff`.
    async fn get_stuck_update_intent_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<i64>>;

    /// Moves a post intent to 'failed' without further retries. Used when the
    /// event exists on the homeserver but was never projected – resending
    /// would create a duplicate comment.
    async fn dead_letter_post_intent(&self, id: i64, error: &str) -> Result<()>;

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
        sender: &str,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<()>;

    /// Updates only the content of a comment.
    async fn update_comment_content(&self, event_id: &str, content: &str) -> Result<bool>;

    /// Deletes a comment by its event ID.
    async fn delete_comment(&self, event_id: &str) -> Result<bool>;

    /// Gets the author nickname for a specific event.
    async fn get_author_nickname(&self, event_id: &str) -> Result<Option<String>>;

    /// Returns the stored author public key for a comment, if any. Used to
    /// authorize edit/delete requests by comparing the presented key.
    async fn get_comment_author_public_key(&self, event_id: &str) -> Result<Option<String>>;
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
#[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
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
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        reply_to: Option<&str>,
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
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        intent_id: Option<i64>,
    ) -> Result<String>;

    /// Redacts a message in a specific room.
    async fn redact_message(
        &self,
        room_id: &str,
        event_id: &str,
        intent_id: Option<i64>,
    ) -> Result<()>;

    /// Fetch one page of room history (CS API `/rooms/{roomId}/messages`).
    async fn get_room_events(
        &self,
        room_id: &str,
        from: Option<&str>,
        limit: u32,
    ) -> Result<RoomEventPage>;

    /// Rooms the service account has joined. Used by backfill to discover
    /// Cumments rooms after a local DB reset.
    async fn joined_rooms(&self) -> Result<Vec<String>>;

    /// Read a room's `host.curious.cumments.metadata` state event, if any.
    async fn room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>>;

    /// Checks whether an event exists on the homeserver. Used to decide if a
    /// timed-out `waiting_for_sync` intent can be safely resent.
    async fn event_exists(&self, room_id: &str, event_id: &str) -> Result<bool>;
}

/// Persistence for backfill cursors (per-room pagination tokens).
#[async_trait]
pub trait BackfillCursorStore: Send + Sync {
    /// The stored pagination token for a room, if a previous backfill
    /// stopped part-way.
    async fn get_cursor(&self, room_id: &str) -> Result<Option<String>>;

    /// Persist the next pagination token for a room.
    async fn save_cursor(&self, room_id: &str, next_batch: &str) -> Result<()>;
}

/// Port for virtual user identity management (AppService mode).
/// Maps Cumments visitor fingerprints to stable Matrix virtual user IDs.
#[async_trait]
pub trait VirtualUserStore: Send + Sync {
    /// Returns the virtual Matrix user ID for the given author public key and site.
    /// Creates one deterministically if it doesn't exist yet.
    ///
    /// Format: `@_cumments_{site_id}_{sha256(public_key) first 8 bytes, hex}:{server_name}`
    async fn get_or_create_virtual_user(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        server_name: &str,
    ) -> Result<String>;
}

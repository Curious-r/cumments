use crate::audit::{CommandAuditEntry, NewCommandAuditEntry};
use crate::commands::{DeleteCommentCommand, PostCommentCommand, UpdateCommentCommand};
use crate::governance::{NewRoleClaim, RoleClaim, RoleEntry};
use crate::media_upload::{
    MediaUploadIdempotency, MediaUploadIdempotencyInput, MediaUploadIdempotencyOutcome,
};
use crate::models::{
    CommentMedia, Message, MessagePage, MessageRevision, PollVote, PostSlug, QuarantinedRoom,
    Reaction, RoomEventPage, RoomIdentity, RoomMember, RoomMetadata, RoomStateEvent, RoomStatus,
    SiteId,
};
use crate::site_auth::{
    NewVerificationToken, Origin, SiteAuthInfo, SiteServiceError, VerificationToken,
};
use crate::sticker_packs::StickerPackProjection;
use crate::submissions::{
    IdempotencyInput, IdempotencyOutcome, PendingDeleteSubmission, PendingPostSubmission,
    PendingUpdateSubmission, StuckDeleteSubmission, StuckPostSubmission, StuckUpdateSubmission,
};
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

/// The port for all submission storage operations.
#[async_trait]
pub trait SubmissionStore: Send + Sync {
    /// Looks up an already-accepted idempotency record without queueing
    /// anything or consuming the request's PoW challenge. Returns `Replayed`
    /// with the original submission ID when the key/fingerprint pair matches,
    /// `Reused` when the key is bound to a different request, and `None`
    /// when the key is free.
    async fn lookup_idempotency(
        &self,
        idempotency: &IdempotencyInput,
    ) -> Result<Option<IdempotencyOutcome>>;

    /// Persists a new post submission and returns its queue row ID.
    async fn save_post_submission(&self, command: &PostCommentCommand) -> Result<i64>;
    /// Persists a new delete submission and returns its queue row ID.
    async fn save_delete_submission(&self, command: &DeleteCommentCommand) -> Result<i64>;
    /// Persists a new update submission and returns its queue row ID.
    async fn save_update_submission(&self, command: &UpdateCommentCommand) -> Result<i64>;

    /// Saves a post submission and its idempotency record atomically.
    ///
    /// The lookup, fingerprint comparison and inserts happen in one
    /// transaction so concurrent retries of the same key cannot queue
    /// duplicate submissions.
    async fn save_post_submission_idempotent(
        &self,
        command: &PostCommentCommand,
        idempotency: &IdempotencyInput,
    ) -> Result<IdempotencyOutcome>;

    /// Saves a delete submission and its idempotency record atomically.
    async fn save_delete_submission_idempotent(
        &self,
        command: &DeleteCommentCommand,
        idempotency: &IdempotencyInput,
    ) -> Result<IdempotencyOutcome>;

    /// Saves an update submission and its idempotency record atomically.
    async fn save_update_submission_idempotent(
        &self,
        command: &UpdateCommentCommand,
        idempotency: &IdempotencyInput,
    ) -> Result<IdempotencyOutcome>;

    /// Returns at most `limit` due pending post submissions, oldest first.
    /// Resets `processing` rows whose lease has expired back to `pending`,
    /// so a crashed reconciler's in-flight work is recovered. Returns how
    /// many rows were recovered.
    async fn recover_expired_submission_leases(&self) -> Result<u64>;

    /// Persists the transaction ID chosen for a post submission's next send.
    /// Must be called before the driver request so a retry after a lost
    /// response reuses the same ID (homeserver-side idempotency).
    async fn set_post_submission_txn_id(&self, id: i64, txn_id: &str) -> Result<()>;

    /// Clears a post submission's transaction ID so its next send allocates a
    /// fresh one.
    /// Called when the timeout pass confirmed the recorded event is absent,
    /// which otherwise would keep reusing an ID that points at a ghost event.
    async fn clear_post_submission_txn_id(&self, id: i64) -> Result<()>;

    /// Persists the transaction ID chosen for a delete submission's next send.
    async fn set_delete_submission_txn_id(&self, id: i64, txn_id: &str) -> Result<()>;

    /// Clears a delete submission's transaction ID after the timeout pass
    /// confirmed its recorded event is absent.
    async fn clear_delete_submission_txn_id(&self, id: i64) -> Result<()>;

    /// Persists the transaction ID chosen for an update submission's next send.
    async fn set_update_submission_txn_id(&self, id: i64, txn_id: &str) -> Result<()>;

    /// Clears an update submission's transaction ID after the timeout pass
    /// confirmed its recorded event is absent.
    async fn clear_update_submission_txn_id(&self, id: i64) -> Result<()>;

    /// Atomically claims up to `limit` due post submissions, oldest first,
    /// marking them `processing` with a lease expiring at `lease_until`.
    async fn claim_pending_post_submissions(
        &self,
        limit: u64,
        lease_until: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PendingPostSubmission>>;
    /// Atomically claims up to `limit` due delete submissions, oldest first.
    async fn claim_pending_delete_submissions(
        &self,
        limit: u64,
        lease_until: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PendingDeleteSubmission>>;
    /// Atomically claims up to `limit` due update submissions, oldest first.
    async fn claim_pending_update_submissions(
        &self,
        limit: u64,
        lease_until: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PendingUpdateSubmission>>;

    /// Transitions a post submission to 'waiting_for_sync' and records the Matrix event ID.
    async fn mark_post_submission_waiting_for_sync(
        &self,
        id: i64,
        event_id: &str,
        room_id: &str,
    ) -> Result<()>;

    /// Completes a post submission by its queue ID (used when the projector sees
    /// the event before the reconciler has written back the Matrix event ID).
    async fn mark_post_submission_completed_by_id(&self, id: i64) -> Result<()>;

    /// Transitions an update submission to 'waiting_for_sync' and records the
    /// replacement event ID.
    async fn mark_update_submission_waiting_for_sync(
        &self,
        id: i64,
        event_id: &str,
        room_id: &str,
    ) -> Result<()>;

    /// Completes a specific update submission by its queue ID. Used when the edit
    /// event carries `host.curious.cumments.submission_id`, so completing one edit
    /// never closes a different queued edit targeting the same original
    /// comment.
    async fn mark_update_submission_completed_by_id(&self, id: i64) -> Result<()>;

    /// Transitions a delete submission to 'waiting_for_sync' and records the
    /// redaction event ID.
    async fn mark_delete_submission_waiting_for_sync(
        &self,
        id: i64,
        event_id: &str,
        room_id: &str,
    ) -> Result<()>;

    /// Records a processing failure. Returns `true` if the submission was
    /// scheduled for another attempt (pending + backoff), `false` if the
    /// retry budget is exhausted and the submission moves to 'failed'.
    async fn record_post_submission_failure(&self, id: i64, error: &str) -> Result<bool>;
    async fn record_delete_submission_failure(&self, id: i64, error: &str) -> Result<bool>;
    async fn record_update_submission_failure(&self, id: i64, error: &str) -> Result<bool>;

    /// Transitions a post submission to 'completed' when the projector sees the event.
    async fn mark_post_submission_completed(&self, event_id: &str) -> Result<()>;

    /// Post submissions stuck in `waiting_for_sync` since before `cutoff`.
    async fn get_stuck_post_submissions(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<StuckPostSubmission>>;

    /// Delete submissions stuck in `waiting_for_sync` since before `cutoff`.
    async fn get_stuck_delete_submissions(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<StuckDeleteSubmission>>;

    /// Update submissions stuck in `waiting_for_sync` since before `cutoff`.
    async fn get_stuck_update_submissions(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<StuckUpdateSubmission>>;

    /// Moves a post submission to 'failed' without further retries. Used when the
    /// event exists on the homeserver but was never projected – resending
    /// would create a duplicate comment.
    async fn dead_letter_post_submission(&self, id: i64, error: &str) -> Result<()>;

    /// Records one more consecutive timeout pass in which the event was
    /// observed on the homeserver. Returns the new confirmation count.
    async fn increment_post_timeout_confirmation(&self, id: i64) -> Result<u32>;

    /// Resets the consecutive timeout-confirmation counter (e.g. after the
    /// event was found absent and the submission was rescheduled).
    async fn reset_post_timeout_confirmations(&self, id: i64) -> Result<()>;

    /// Records one more consecutive timeout-pass error (network/homeserver
    /// failure while checking event existence). Returns the new count.
    async fn increment_post_timeout_error(&self, id: i64) -> Result<u32>;

    /// Resets the consecutive timeout-error counter after a successful check.
    async fn reset_post_timeout_errors(&self, id: i64) -> Result<()>;

    /// Transitions a delete submission to 'completed' when the projector sees the redaction.
    async fn mark_delete_submission_completed(&self, target_event_id: &str) -> Result<()>;

    /// Transitions update submissions for `event_id` to 'completed' when the
    /// projector sees the replacement. The fallback is scoped to the projected
    /// author key so an external edit cannot close unrelated queued submissions.
    async fn mark_update_submission_completed(
        &self,
        event_id: &str,
        author_public_key: Option<&str>,
    ) -> Result<()>;
}

/// The port for all message projection storage operations.
#[async_trait]
pub trait MessageStore: Send + Sync {
    /// Fetches a single message by its Matrix event ID.
    async fn get_message(&self, event_id: &str) -> Result<Option<Message>>;

    /// Fetches one page of projected messages for a given site and post.
    async fn get_messages(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        limit: i64,
        offset: i64,
    ) -> Result<MessagePage>;

    /// Saves a new message or updates an existing one (on conflict by
    /// event_id).
    async fn save_message(&self, message: &Message) -> Result<()>;

    /// Applies an edit to a message and records the revision. Bound to the
    /// room the edit arrived from and ordered by edit recency. Returns
    /// `false` when the target is missing, lives in another room, or the edit
    /// is older than the content already stored.
    async fn apply_edit(&self, message: &Message, revision: &MessageRevision) -> Result<bool>;

    /// Marks a message as redacted (kept in the read model as a tombstone).
    /// Returns `false` when the message is missing or lives in another room.
    async fn redact_message(
        &self,
        event_id: &str,
        room_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
        redacted_by: &str,
    ) -> Result<bool>;

    /// Gets the author display name for a specific event.
    /// `None` means the message is missing; `Some(None)` means the message
    /// exists without a display name; `Some(Some(name))` is a stored name.
    async fn get_author_display_name(&self, event_id: &str) -> Result<Option<Option<String>>>;

    /// Returns the stored author public key for a message, if any. Used to
    /// authorize edit/delete requests by comparing the presented key.
    async fn get_author_public_key(&self, event_id: &str) -> Result<Option<String>>;

    /// Saves or updates a reaction event (upsert by its own event ID).
    async fn save_reaction(&self, reaction: &Reaction) -> Result<()>;

    /// Looks up a stored reaction by its Matrix event ID.
    async fn get_reaction(&self, event_id: &str) -> Result<Option<Reaction>>;

    /// Marks a reaction event as redacted.
    async fn redact_reaction(
        &self,
        event_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool>;

    /// Records a poll vote (upsert per voter; the latest vote wins).
    async fn save_poll_vote(&self, vote: &PollVote) -> Result<()>;

    /// Looks up a stored poll vote by its Matrix event ID.
    async fn get_poll_vote_by_event(&self, event_id: &str) -> Result<Option<PollVote>>;

    /// Marks a poll vote as redacted (removed from the aggregate).
    async fn redact_poll_vote(
        &self,
        event_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
        redacted_by: &str,
    ) -> Result<bool>;

    /// Records a guest upload so comment submissions can later prove ownership.
    async fn record_media_upload(
        &self,
        mxc_url: &str,
        author_public_key: &str,
        site_id: &str,
        post_slug: &str,
    ) -> Result<()>;

    /// Whether a media URL was uploaded by this author for this site/post.
    async fn media_upload_owned_by(
        &self,
        mxc_url: &str,
        author_public_key: &str,
        site_id: &str,
        post_slug: &str,
    ) -> Result<bool>;

    /// Marks a media URL as referenced by a a comment submission.
    async fn mark_media_used(&self, mxc_url: &str) -> Result<()>;

    /// MXC URLs uploaded before `cutoff` that are still unreferenced.
    async fn list_unused_media_before(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<String>>;

    /// Removes the local upload record (after the homeserver copy was
    /// deleted or is unreachable).
    async fn delete_media_upload(&self, mxc_url: &str) -> Result<()>;

    /// Lists every recorded media MXC URL for one site, used by decommission
    /// to delete the homeserver copies before the rows are dropped.
    async fn list_media_urls_for_site(&self, site_id: &str) -> Result<Vec<String>>;

    /// Returns an unexpired upload idempotency record, if one exists.
    async fn find_media_upload_idempotency(
        &self,
        author_public_key: &str,
        idempotency_key: &str,
    ) -> Result<Option<MediaUploadIdempotency>>;

    /// Atomically records the upload ownership row and its idempotency key.
    /// On a concurrent key race the loser's upload is rolled back and the
    /// winner's URL is returned.
    async fn save_media_upload_idempotent(
        &self,
        mxc_url: &str,
        author_public_key: &str,
        site_id: &str,
        post_slug: &str,
        idempotency: &MediaUploadIdempotencyInput,
    ) -> Result<MediaUploadIdempotencyOutcome>;

    /// Persists a tombstone for a redacted event whose original has not been
    /// projected yet (or may be re-delivered), so the message cannot
    /// resurrect after a capped/resumed backfill or a push retry.
    async fn record_backfill_tombstone(
        &self,
        event_id: &str,
        room_id: &str,
        redaction_event_id: &str,
    ) -> Result<()>;

    /// Whether a tombstone exists for this event in this room.
    async fn has_backfill_tombstone(&self, event_id: &str, room_id: &str) -> Result<bool>;
}

/// The port for room-level metadata: member profiles and the system-message
/// (state event) feed. Independent from the message read model.
#[async_trait]
pub trait RoomStore: Send + Sync {
    /// Upserts a room member profile.
    async fn save_member(&self, member: &RoomMember) -> Result<()>;

    /// Looks up a member profile by room and user.
    async fn get_member(&self, room_id: &str, user_id: &str) -> Result<Option<RoomMember>>;

    /// Stores one room state event (idempotent by event ID).
    async fn save_state_event(&self, event: &RoomStateEvent) -> Result<()>;

    /// Looks up one stored state event by its Matrix event ID.
    async fn get_state_event(&self, event_id: &str) -> Result<Option<RoomStateEvent>>;

    /// Replaces the stored content of a state event (e.g. after a redaction
    /// stripped it per the room-version algorithm). Returns `false` when the
    /// event is not stored.
    async fn update_state_event_content(
        &self,
        event_id: &str,
        content: &serde_json::Value,
    ) -> Result<bool>;

    /// The latest stored state event for a `(room, type, state_key)` slot.
    async fn get_latest_state_event(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
    ) -> Result<Option<RoomStateEvent>>;

    /// Derives the current room metadata from the latest state events.
    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<RoomMetadata>>;

    /// Returns the most recent state events for a room (system-message feed).
    async fn get_room_system_messages(
        &self,
        room_id: &str,
        limit: i64,
    ) -> Result<Vec<RoomStateEvent>>;
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

    /// Returns the lifecycle status of a room, if it is in the registry.
    async fn get_room_status(&self, room_id: &str) -> Result<Option<RoomStatus>>;

    /// Lists all rooms currently registered as active (canonical).
    async fn list_active_rooms(&self) -> Result<Vec<String>>;

    /// Lists the active room IDs registered for one site.
    async fn list_active_rooms_for_site(&self, site_id: &SiteId) -> Result<Vec<String>>;

    /// Lists every room registered for one site, regardless of lifecycle
    /// status. Used by decommission so quarantined/superseded rooms are
    /// retired too.
    async fn list_rooms_for_site(&self, site_id: &SiteId) -> Result<Vec<String>>;

    /// Lists every superseded room. Used by the room-cleanup pass to retire
    /// AS-managed memberships from rooms that were replaced.
    async fn list_superseded_rooms(&self) -> Result<Vec<String>>;

    /// Looks up the Cumments identity registered for a room.
    ///
    /// Unlike [`Self::get_registered_room`] this is a reverse lookup by room ID,
    /// used by the projector to resolve the context of incoming Matrix events
    /// without depending on room state metadata.
    async fn get_registered_room_identity(&self, room_id: &str) -> Result<Option<RoomIdentity>>;

    /// Registers or reactivates a room in the registry.
    async fn register_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<()>;

    /// Registers a room only when it is not already in the registry, without
    /// changing the lifecycle status of existing rows. Used by backfill so
    /// quarantined or superseded rooms are not silently resurrected.
    async fn register_room_if_absent(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<()>;

    /// Retires a room from the registry (e.g. the room no longer exists or
    /// was replaced), keeping the row for projection history.
    async fn retire_room(&self, room_id: &str) -> Result<()>;

    /// Quarantines a room after an adoption failure. `adoption_failures` is
    /// the failure count after this attempt (the caller derives it from the
    /// backoff policy); the original quarantine time is preserved when the
    /// room is already quarantined. `next_attempt_at` schedules the next
    /// automatic adoption attempt; `None` means the room needs manual
    /// attention (`reinstate_room`).
    async fn quarantine_room(
        &self,
        room_id: &str,
        reason: &str,
        adoption_failures: u32,
        next_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()>;

    /// Clears a room's quarantine and makes it the canonical room again.
    /// Returns `false` when the room is not in the registry; reinstating an
    /// already-active room is a successful no-op.
    async fn reinstate_room(&self, room_id: &str) -> Result<bool>;

    /// Lists all rooms currently quarantined from adoption.
    async fn get_quarantined_rooms(&self) -> Result<Vec<QuarantinedRoom>>;
}

/// Defines the operations for managing sites in the local database.
#[async_trait]
pub trait SiteStore: Send + Sync {
    async fn get_site(&self, id: &SiteId) -> Result<Option<crate::models::Site>>;

    /// Looks up a site by its Matrix Space room ID.
    async fn get_site_by_space_id(&self, space_id: &str) -> Result<Option<crate::models::Site>>;

    /// Lists every site known to the local store.
    async fn list_sites(&self) -> Result<Vec<crate::models::Site>>;

    async fn save_site(&self, site: &crate::models::Site) -> Result<()>;

    /// Ensures a site exists in the database, creating it with default values if not.
    async fn ensure_site_exists(&self, site_id: &str, matrix_space_id: &str) -> Result<()>;
}

/// Port for the projected governance read model.
///
/// The authoritative state lives in Matrix power levels; these rows are a
/// disposable projection used for API visibility and offline inspection, and
/// are rebuilt by pushes and `cumments backfill`.
#[async_trait]
pub trait GovernanceStore: Send + Sync {
    /// Atomically replaces the projected roles of one site (from its Space).
    async fn replace_site_roles(&self, site_id: &str, roles: &[RoleEntry]) -> Result<()>;

    /// The projected roles of one site, ordered by user ID.
    async fn list_site_roles(&self, site_id: &str) -> Result<Vec<RoleEntry>>;

    /// Atomically replaces the projected roles of one comment room.
    async fn replace_room_roles(&self, room_id: &str, roles: &[RoleEntry]) -> Result<()>;

    /// The projected roles of one comment room, ordered by user ID.
    async fn list_room_roles(&self, room_id: &str) -> Result<Vec<RoleEntry>>;
}

/// Port for the projected sticker-pack read model.
///
/// The authoritative data lives in `m.room.image_pack` state events on a
/// site's Space; these rows are a disposable projection rebuilt by pushes
/// and `cumments backfill`.
#[async_trait]
pub trait StickerPackStore: Send + Sync {
    /// Upserts one site pack (latest state event wins per site + state key).
    async fn save_site_pack(&self, pack: &StickerPackProjection) -> Result<()>;

    /// All projected packs for one site, ordered by pack id.
    async fn list_site_packs(&self, site_id: &str) -> Result<Vec<StickerPackProjection>>;

    /// One projected pack by site and pack id (state key).
    async fn get_site_pack(
        &self,
        site_id: &str,
        state_key: &str,
    ) -> Result<Option<StickerPackProjection>>;

    /// Removes the projected pack (e.g. its state event was redacted).
    async fn delete_site_pack(&self, site_id: &str, state_key: &str) -> Result<()>;

    /// Finds the (site, pack id) projected from a given Matrix event id,
    /// used to decide whether a redaction affects the current pack.
    async fn find_pack_by_event_id(&self, event_id: &str) -> Result<Option<(String, String)>>;
}

/// Port for token-DM role claims: short-lived process state between role
/// registration and the target MXID proving ownership.
#[async_trait]
pub trait RoleClaimStore: Send + Sync {
    /// Creates a pending claim or rotates the token of an existing claim for
    /// the same (site, room, user, level) scope.
    async fn upsert_role_claim(&self, claim: &NewRoleClaim) -> Result<RoleClaim>;

    /// Pending claims whose target MXID is `user_id` and which have not
    /// expired yet.
    async fn pending_claims_for_user(&self, user_id: &str) -> Result<Vec<RoleClaim>>;

    /// Transitions a pending claim to `activated`. Returns `false` when the
    /// claim was missing, expired or already in another state.
    async fn mark_claim_activated(&self, id: i64) -> Result<bool>;

    /// Claims that proved ownership but have not been written to Matrix yet.
    async fn activated_unapplied_claims(&self) -> Result<Vec<RoleClaim>>;

    /// Marks an activated claim as applied.
    async fn mark_claim_applied(&self, id: i64) -> Result<()>;

    /// Cancels a claim that has not been applied yet. Returns `false` when no
    /// cancellable claim exists (e.g. it was already applied).
    async fn revoke_role_claim(
        &self,
        site_id: &str,
        room_id: &str,
        user_id: &str,
        level: i64,
    ) -> Result<bool>;

    /// Marks an applied claim as revoked after its Matrix role was removed.
    /// Returns `false` when no applied claim exists for the key.
    async fn mark_applied_claim_revoked(
        &self,
        site_id: &str,
        room_id: &str,
        user_id: &str,
        level: i64,
    ) -> Result<bool>;

    /// Every applied claim whose Matrix role should still exist. Used by the
    /// background auditor to converge claim rows with projected power levels.
    async fn list_applied_claims(&self) -> Result<Vec<RoleClaim>>;

    /// Records the DM room the bot joined for a user's pending claims.
    async fn set_claim_dm_room_for_user(&self, user_id: &str, room_id: &str) -> Result<()>;

    /// Whether any claim references this room as its verification DM.
    async fn claim_dm_room_exists(&self, room_id: &str) -> Result<bool>;

    /// Distinct `(user_id, dm_room_id)` pairs recorded for claim DMs.
    async fn claim_dm_rooms(&self) -> Result<Vec<(String, String)>>;

    /// Whether the user still has a pending or activated claim verified in
    /// this DM room. Used to decide when the bot may leave.
    async fn active_claims_in_dm_room(&self, user_id: &str, room_id: &str) -> Result<bool>;

    /// Distinct `(user_id, dm_room_id)` pairs recorded for one site's claims.
    /// Used by decommission so the bot leaves verification DMs after the
    /// site's claims are deleted.
    async fn claim_dm_rooms_for_site(&self, site_id: &str) -> Result<Vec<(String, String)>>;

    /// Deletes expired claims that never reached `applied`. Applied claims
    /// are kept for audit purposes.
    async fn purge_expired_claims(&self) -> Result<u64>;
}

/// Port for site identity and write-path authentication state.
#[async_trait]
pub trait SiteAuthStore: Send + Sync {
    /// Creates a site row for an API-registered site (unverified, origin
    /// auth). Fails with [`SiteServiceError::SiteAlreadyExists`] when the
    /// generated site ID collides with an existing row.
    async fn register_site(
        &self,
        site_id: &str,
        claim_token_hash: &str,
        custom_id: bool,
    ) -> Result<(), SiteServiceError>;

    /// Full authentication state of a site, if a row exists.
    async fn get_site_auth(&self, site_id: &str) -> Result<Option<SiteAuthInfo>>;

    /// Transitions an `active` site to `retiring`, clearing its claim token
    /// so ownership proofs stop working. Returns `false` when the site does
    /// not exist or is not `active`.
    async fn mark_site_retiring(&self, site_id: &str) -> Result<bool>;

    /// Site ids whose decommission has been requested but not finished.
    async fn list_retiring_sites(&self) -> Result<Vec<String>>;

    /// Removes every local trace of a decommissioned site (auth row,
    /// projections, rooms, submissions). Callers must have already retired the
    /// Matrix side; this is the final, idempotent cleanup.
    async fn delete_site(&self, site_id: &str) -> Result<()>;

    /// Stored SHA-256 hash of the site's claim token, if any.
    async fn get_claim_token_hash(&self, site_id: &str) -> Result<Option<String>>;

    /// Replaces the stored claim-token hash. Returns `false` when the site
    /// does not exist.
    async fn rotate_claim_token(&self, site_id: &str, new_hash: &str) -> Result<bool>;

    /// Inserts the rows of one verification challenge (same raw token, one
    /// row per origin).
    async fn insert_verification_tokens(&self, tokens: &[NewVerificationToken]) -> Result<()>;

    /// Returns an unconsumed, unexpired verification token row, if any.
    async fn find_verification_token(
        &self,
        site_id: &str,
        origin: &Origin,
        token_hash: &str,
    ) -> Result<Option<VerificationToken>>;

    /// Increments the attempt counter for a verification token and returns
    /// the new count. Used to cap outbound proof probes per token.
    async fn increment_verification_attempt(&self, id: i64) -> Result<u32>;

    /// Marks a verification token row consumed. Returns `false` if it was
    /// already consumed (e.g. by a concurrent confirmation).
    async fn consume_verification_token(&self, id: i64) -> Result<bool>;

    /// Records a verified origin and marks the site verified.
    async fn add_verified_origin(&self, site_id: &str, origin: &Origin) -> Result<()>;

    /// Atomically consumes a verification token and records the verified
    /// origin. Returns `false` when the token was already consumed (for
    /// example by a concurrent confirmation); the origin is still recorded.
    async fn complete_verification(
        &self,
        site_id: &str,
        origin: &Origin,
        token_id: i64,
    ) -> Result<bool>;

    /// Stores the site's HMAC key and switches the site to secret auth.
    /// The key is needed in plain form to verify HMAC signatures.
    async fn store_site_secret(&self, site_id: &str, secret: &str) -> Result<()>;

    /// Lists every database-tracked site with its authentication state.
    async fn list_site_auth(&self) -> Result<Vec<SiteAuthInfo>>;

    /// Removes a verified origin. Returns `false` when the origin was not
    /// present; when the last origin is removed the site falls back to
    /// `unverified`.
    async fn revoke_verified_origin(&self, site_id: &str, origin: &Origin) -> Result<bool>;

    /// Removes the HMAC key and switches the site back to origin auth.
    /// Returns `false` when the site does not exist.
    async fn clear_site_secret(&self, site_id: &str) -> Result<bool>;
}

/// Defines the atomic actions that can be performed on the Matrix network.
/// This is the "Hands" of the system.
///
/// This trait is the only seam through which Cumments-initiated homeserver
/// writes may pass. Write methods perform the wire operation only; callers
/// own the application policy (PoW, signatures, authorization, size/MIME
/// checks, rate limits, validation). Read methods are also part of this
/// trait but are not covered by the write-seam invariant.
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

    /// Renames a room via the `m.room.name` state event.
    async fn set_room_name(&self, room_id: &str, name: &str) -> Result<()>;

    /// Removes the AS sender from a room. Rooms already left (or unknown to
    /// the homeserver) are treated as success.
    async fn leave_room(&self, room_id: &str) -> Result<()>;

    /// Removes a specific AS-managed user (e.g. a guest virtual user) from a
    /// room. Rooms the user is not in are treated as success.
    async fn leave_room_as(&self, room_id: &str, user_id: &str) -> Result<()>;

    /// Joins a room as the AS sender. Used to accept claim-DM invites after
    /// the conditional auto-join gate passes. Already-joined rooms are a
    /// successful no-op.
    async fn join_room(&self, room_id: &str) -> Result<()>;

    /// Deletes the site's Space alias (`post_slug: None`) or one comment
    /// room's alias from the room directory. Missing aliases are a no-op.
    async fn remove_room_alias(&self, site_id: &SiteId, post_slug: Option<&PostSlug>)
    -> Result<()>;

    /// Best-effort deletion of one media item on the homeserver. Returns
    /// `true` when the homeserver confirmed the deletion (or the item was
    /// already gone), so the caller can forget the local upload record;
    /// `false` and errors mean the record should be kept for a later sweep.
    async fn delete_media(&self, server: &str, media_id: &str) -> Result<bool>;

    /// Uploads media to the homeserver as the author's virtual user and
    /// returns the `mxc://` content URI.
    ///
    /// Callers must verify the PoW challenge and author signature before
    /// calling this; the driver resolves the virtual user and performs the
    /// upload.
    async fn upload_media(
        &self,
        bytes: Bytes,
        filename: &str,
        mimetype: &str,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<String>;

    /// Posts a message to a specific room.
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        media: Option<&CommentMedia>,
        display_name: &str,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        reply_to: Option<&str>,
        // The plain-text body and sender MXID of the replied-to event, used to
        // build the rich-reply fallback quote for clients without relation
        // support. `None` when the original event is unknown.
        reply_to_body: Option<&str>,
        reply_to_sender: Option<&str>,
        // Correlation hint: the submission queue row ID, published in the event so
        // the projector can close the loop even if the push arrives before the
        // reconciler's write-back.
        submission_id: Option<i64>,
        // The exact transaction ID to use for this attempt. It is persisted on
        // the submission row so retries reuse it; a confirmed-absent event
        // clears it and the reconciler allocates a fresh one.
        txn_id: &str,
    ) -> Result<String>;

    /// Sends a reaction (`m.reaction`) as the guest's virtual user.
    async fn react_message(
        &self,
        room_id: &str,
        target_event_id: &str,
        key: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
    ) -> Result<()>;

    /// Sends a poll vote (`m.poll.response`) as the guest's virtual user.
    async fn vote_poll(
        &self,
        room_id: &str,
        poll_event_id: &str,
        answer_id: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
    ) -> Result<()>;

    /// Sends a location message (`m.location`, MSC3488) as the guest's
    /// virtual user. Returns the Matrix event ID and carries the submission
    /// correlation hint, like [`Self::post_message`].
    async fn post_location(
        &self,
        room_id: &str,
        geo_uri: &str,
        description: Option<&str>,
        display_name: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        submission_id: Option<i64>,
        // See [`Self::post_message`].
        txn_id: &str,
    ) -> Result<String>;

    /// Updates an existing message in a specific room using m.replace.
    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        display_name: &str,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        submission_id: Option<i64>,
        // See [`Self::post_message`].
        txn_id: &str,
    ) -> Result<String>;

    /// Redacts a message in a specific room.
    ///
    /// `proof` is an optional machine-readable Cumments block published in
    /// the redaction reason so the delete authorization (public key,
    /// signature, challenge) stays verifiable from Matrix alone.
    async fn redact_message(
        &self,
        room_id: &str,
        event_id: &str,
        submission_id: Option<i64>,
        proof: Option<&serde_json::Value>,
        // See [`Self::post_message`].
        txn_id: &str,
    ) -> Result<String>;

    /// Fetch one page of room history (CS API `/rooms/{roomId}/messages`).
    async fn get_room_events(
        &self,
        room_id: &str,
        from: Option<&str>,
        limit: u32,
    ) -> Result<RoomEventPage>;

    /// Rooms the service account has joined. Used by backfill to discover
    /// Cumments rooms after a local DB reset.
    async fn get_joined_rooms(&self) -> Result<Vec<String>>;

    /// The joined member MXIDs of a room, queried as the AS sender. Used to
    /// verify a "private channel" (exactly the bot and one other user) before
    /// acting on sensitive chat commands.
    async fn get_joined_members(&self, room_id: &str) -> Result<Vec<String>>;

    /// Sends a plain-text message in a room as the AS sender (the bot).
    /// Returns the event ID. Used for chat command replies.
    async fn send_bot_message(&self, room_id: &str, body: &str) -> Result<String>;

    /// Read a room's `host.curious.cumments.metadata` state event, if any.
    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>>;

    /// Read a room's canonical alias (`m.room.canonical_alias`), if any.
    async fn get_room_canonical_alias(&self, room_id: &str) -> Result<Option<String>>;

    /// Checks whether an event exists on the homeserver. Used to decide if a
    /// timed-out `waiting_for_sync` submission can be safely resent.
    async fn event_exists(&self, room_id: &str, event_id: &str) -> Result<bool>;

    /// The AppService sender user ID, or `None` when this driver has no
    /// Matrix sender account (e.g. logging mode).
    fn sender_user_id(&self) -> Option<String>;

    /// Read a room's current `m.room.power_levels` content, if the state
    /// event exists.
    async fn get_room_power_levels(&self, room_id: &str) -> Result<Option<serde_json::Value>>;

    /// Replace a room's `m.room.power_levels` state event.
    async fn set_room_power_levels(&self, room_id: &str, content: &serde_json::Value)
    -> Result<()>;

    /// Read one room state event's current content (`404` -> `None`).
    async fn get_room_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
    ) -> Result<Option<serde_json::Value>>;

    /// Write one room state event as the AppService sender (full-state
    /// replacement) and return the new event ID.
    async fn set_room_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
        content: &serde_json::Value,
    ) -> Result<String>;

    /// Invites a user to a room as the AS sender. Already-joined users are a
    /// successful no-op.
    async fn invite_user(&self, room_id: &str, user_id: &str) -> Result<()>;
}

/// Persistence for backfill cursors (per-room pagination tokens).
#[async_trait]
pub trait BackfillCursorStore: Send + Sync {
    /// The stored pagination token for a room, if a previous backfill
    /// stopped part-way.
    async fn get_cursor(&self, room_id: &str) -> Result<Option<String>>;

    /// Persist the next pagination token for a room.
    async fn save_cursor(&self, room_id: &str, next_token: &str) -> Result<()>;
}

/// Persistence for the chat command audit log.
#[async_trait]
pub trait CommandAuditStore: Send + Sync {
    /// Records one chat command outcome.
    async fn record_command_audit(&self, entry: &NewCommandAuditEntry) -> Result<()>;

    /// Lists recorded commands, optionally filtered by actor, newest first.
    async fn list_command_audit(
        &self,
        actor_mxid: Option<&str>,
        limit: u64,
    ) -> Result<Vec<CommandAuditEntry>>;
}

/// Port for virtual user identity management (AppService mode).
/// Maps Cumments guest public keys to stable Matrix virtual user IDs.
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

    /// Lists every virtual Matrix user ID recorded for one site.
    async fn list_virtual_users_for_site(&self, site_id: &SiteId) -> Result<Vec<String>>;
}

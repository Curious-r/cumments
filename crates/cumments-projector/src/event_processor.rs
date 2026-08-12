//! Pure event processing logic – independent of how events are received.
//!
//! This module defines the core "projection" functions that transform
//! Matrix events into local read-model updates. It does **not** depend
//! on `matrix_sdk` or any transport-specific types. The AppService
//! `PushReceiver` (and any future transport) calls into these same
//! functions.

use crate::parsed::{ParsedRoomMessage, ParsedRoomRedaction, ParsedSpaceChild};
use crate::verification::{verify_delete_proof, verify_guest_event};
use anyhow::Result;
use cumments_core::{
    identity::{post_signature_message, signature_message},
    models::{AuthorType, Comment, CommentAuthor, PostSlug, RoomIdentity, SiteId},
    ports::{CommentStore, IntentStore, RegistryStore, SiteStore},
    projector_events::ProjectorEvent,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, instrument, warn};

// ── Core processing functions ─────────────────────────────────────

/// The central processor – holds only abstract store references.
pub struct EventProcessor {
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
    comment_store: Arc<dyn CommentStore>,
    intent_store: Arc<dyn IntentStore>,
    event_bus: broadcast::Sender<ProjectorEvent>,
    server_name: Option<String>,
}

impl EventProcessor {
    pub fn new(
        site_store: Arc<dyn SiteStore>,
        registry_store: Arc<dyn RegistryStore>,
        comment_store: Arc<dyn CommentStore>,
        intent_store: Arc<dyn IntentStore>,
        event_bus: broadcast::Sender<ProjectorEvent>,
        server_name: Option<String>,
    ) -> Self {
        Self {
            site_store,
            registry_store,
            comment_store,
            intent_store,
            event_bus,
            server_name,
        }
    }

    /// Look up the site ID associated with a Matrix Space room ID.
    /// Returns `None` if the space is not in our local database.
    pub async fn get_site_id_by_space_id(&self, space_id: &str) -> Result<Option<String>> {
        self.site_store
            .get_site_by_space_id(space_id)
            .await
            .map(|site| site.map(|s| s.id))
    }

    /// Resolve the Cumments identity of a room from the local registry.
    ///
    /// This is the AppService-mode counterpart of reading room state metadata:
    /// the reconciler writes the room mapping back to the registry when it
    /// creates or adopts a room, so incoming push events can be attributed
    /// without any extra homeserver API call.
    pub async fn resolve_room_identity(&self, room_id: &str) -> Result<Option<RoomIdentity>> {
        self.registry_store
            .get_registered_room_identity(room_id)
            .await
    }

    /// Process a room message (new comment or edit).
    #[instrument(skip(self))]
    pub async fn process_room_message(&self, event: ParsedRoomMessage) -> Result<()> {
        // ── PRINCIPLE B: REGISTRY ENFORCEMENT ──
        let registry_status = self.registry_store.is_room_active(&event.room_id).await?;

        match registry_status {
            Some(true) => {
                // Room is active, proceed normally
            }
            Some(false) => {
                // Room is explicitly INACTIVE (tombstoned).
                debug!("Ignoring message from deactivated room {}", event.room_id);
                return Ok(());
            }
            None => {
                // Push events carry no room state, so an unknown room has no
                // identity to register from; rooms are registered ahead of
                // event processing by the reconciler, space-child discovery,
                // or backfill.
                debug!("Ignoring message from unregistered room {}", event.room_id);
                return Ok(());
            }
        }

        // 0. Identify the room context
        let (site_id, post_slug) = match event.room_identity {
            Some(ref id)
                if SiteId::new(id.site_id.clone()).is_ok()
                    && PostSlug::new(id.post_slug.clone()).is_ok() =>
            {
                (id.site_id.clone(), id.post_slug.clone())
            }
            Some(ref id) => {
                warn!(
                    "Ignoring message from room {} with invalid identity {}/{}",
                    event.room_id, id.site_id, id.post_slug
                );
                return Ok(());
            }
            None => return Ok(()), // Not a cumments room
        };

        // B-04: a redaction may have been seen before its target (capped or
        // resumed backfill, push retry). Never re-project a tombstoned event.
        if self
            .comment_store
            .has_backfill_tombstone(&event.event_id, &event.room_id)
            .await?
        {
            debug!("Ignoring tombstoned event {}", event.event_id);
            return Ok(());
        }

        // Handle Edits (Replacements)
        if let Some(ref relation) = event.relates_to {
            info!("Handling edit for event {}", relation.target_event_id);

            // Integrity: Matrix does not enforce same-sender on m.replace, so
            // verify the replacement was sent by the original comment's author
            // virtual user. Legacy rows without a recorded sender are accepted
            // until re-projected by backfill.
            if let Some(existing) = self
                .comment_store
                .get_comment(&relation.target_event_id)
                .await?
                && !existing.sender_mxid.is_empty()
                && existing.sender_mxid != event.sender
            {
                warn!(
                    "Rejecting edit for {} from {}: sender does not match original author {}",
                    relation.target_event_id, event.sender, existing.sender_mxid
                );
                return Ok(());
            }

            // Guest edits must carry a valid Cumments identity block and
            // signature; Matrix-native edits are governed by the sender check
            // above.
            if event.is_virtual_user_sender {
                let valid = match (
                    &event.author_public_key,
                    &event.author_signature,
                    &event.author_challenge,
                ) {
                    (Some(pk), Some(sig), Some(chal)) => {
                        let message = signature_message(&[
                            "PATCH",
                            &site_id,
                            &post_slug,
                            &relation.target_event_id,
                            &relation.new_content,
                            chal,
                        ]);
                        verify_guest_event(
                            self.server_name.as_deref(),
                            &event.sender,
                            &site_id,
                            pk,
                            sig,
                            &message,
                        )
                    }
                    _ => false,
                };
                if !valid {
                    warn!(
                        "Rejecting guest edit for {} from {}: missing or invalid Cumments identity block",
                        relation.target_event_id, event.sender
                    );
                    return Ok(());
                }
            }

            if self
                .comment_store
                .update_comment_content(
                    &relation.target_event_id,
                    &event.room_id,
                    &relation.new_content,
                    event.origin_server_ts,
                    &event.event_id,
                )
                .await?
            {
                info!("Successfully updated comment {}", relation.target_event_id);
                // Closed-loop only after the projection succeeded: the
                // correlation ID lets concurrent edits close independently;
                // legacy events fall back to target-event matching (waiting
                // intents only). A failed projection leaves the intent open
                // for the timeout/backfill safety net.
                match event.intent_id {
                    Some(id) => {
                        self.intent_store
                            .mark_update_intent_completed_by_id(id)
                            .await?
                    }
                    None => {
                        self.intent_store
                            .mark_update_intent_completed(&relation.target_event_id)
                            .await?
                    }
                };
                if let Some(comment) = self
                    .comment_store
                    .get_comment(&relation.target_event_id)
                    .await?
                {
                    let _ = self.event_bus.send(ProjectorEvent::CommentUpdated {
                        site_id,
                        post_slug,
                        comment,
                    });
                }
            } else {
                debug!(
                    "Edit ignored for {}: target missing, in another room, or stale",
                    relation.target_event_id
                );
            }
            return Ok(());
        }

        // Guest posts must carry a valid Cumments identity block and
        // signature. Matrix-native posts skip this path entirely: their
        // identity is the Matrix sender itself.
        if event.is_virtual_user_sender {
            let valid = match (
                &event.author_public_key,
                &event.author_signature,
                &event.author_challenge,
                &event.display_name,
            ) {
                (Some(pk), Some(sig), Some(chal), Some(nick)) => {
                    let message = post_signature_message(
                        &site_id,
                        &post_slug,
                        &event.content,
                        nick,
                        event.reply_to.as_deref(),
                        chal,
                    );
                    verify_guest_event(
                        self.server_name.as_deref(),
                        &event.sender,
                        &site_id,
                        pk,
                        sig,
                        &message,
                    )
                }
                _ => false,
            };
            if !valid {
                warn!(
                    "Rejecting guest post {} from {}: missing or invalid Cumments identity block",
                    event.event_id, event.sender
                );
                return Ok(());
            }
        }

        // Handle Original Posts
        let is_matrix_native = !event.is_virtual_user_sender;
        let comment = Comment {
            event_id: event.event_id.clone(),
            site_id: site_id.clone(),
            post_slug: post_slug.clone(),
            author: CommentAuthor {
                kind: if is_matrix_native {
                    AuthorType::Matrix
                } else {
                    AuthorType::Guest
                },
                display_name: event.display_name.clone(),
                public_key: event.author_public_key.clone(),
                mxid: if is_matrix_native {
                    Some(event.sender.clone())
                } else {
                    None
                },
            },
            content: event.content.clone(),
            timestamp: chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
                .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
            reply_to: event.reply_to.clone(),
            room_id: event.room_id.clone(),
            sender_mxid: event.sender.clone(),
        };

        self.comment_store
            .save_comment(
                &comment,
                &event.room_id,
                &event.sender,
                &site_id.clone().into(),
                &post_slug.clone().into(),
            )
            .await?;
        info!("Successfully projected comment event {}", event.event_id);
        // Closed-loop only after the projection succeeded. Prefer the
        // correlation ID when present – the push may arrive before the
        // reconciler's write-back, so the event_id is not yet stored on the
        // intent row. Fall back to event_id matching for external messages.
        match event.intent_id {
            Some(id) => {
                self.intent_store
                    .mark_post_intent_completed_by_id(id)
                    .await?
            }
            None => {
                self.intent_store
                    .mark_post_intent_completed(&event.event_id)
                    .await?
            }
        };
        let _ = self.event_bus.send(ProjectorEvent::CommentCreated {
            site_id,
            post_slug,
            comment,
        });
        Ok(())
    }

    /// Process a redaction event (comment deletion).
    #[instrument(skip(self))]
    pub async fn process_room_redaction(&self, event: ParsedRoomRedaction) -> Result<()> {
        let target_event_id = match event.redacts {
            Some(ref id) => id.clone(),
            None => {
                debug!("Redaction event without a target event_id, ignoring");
                return Ok(());
            }
        };

        // Same registry gate as message processing: ignore redactions from
        // deactivated or unregistered rooms so tombstoned rooms cannot keep
        // mutating the read model through live pushes.
        match self.registry_store.is_room_active(&event.room_id).await? {
            Some(true) => {}
            Some(false) => {
                debug!("Ignoring redaction from deactivated room {}", event.room_id);
                return Ok(());
            }
            None => {
                debug!(
                    "Ignoring redaction from unregistered room {}",
                    event.room_id
                );
                return Ok(());
            }
        }

        info!(
            "Handling redaction for event {} in room {}",
            target_event_id, event.room_id
        );

        // Integrity: only redact a comment that actually lives in the room the
        // redaction arrived from. Fetch before deleting so the check uses the
        // same snapshot the deletion will operate on.
        let comment = self.comment_store.get_comment(&target_event_id).await?;

        if let Some(c) = &comment {
            if c.room_id != event.room_id {
                warn!(
                    "Ignoring redaction for {} in {}: comment lives in {}",
                    target_event_id, event.room_id, c.room_id
                );
                return Ok(());
            }
            if let Some(ref identity) = event.room_identity
                && (c.site_id != identity.site_id || c.post_slug != identity.post_slug)
            {
                warn!(
                    "Ignoring redaction for {} in {}: comment belongs to {}/{}",
                    target_event_id, event.room_id, c.site_id, c.post_slug
                );
                return Ok(());
            }

            // Deletions issued through the Cumments API embed a signed proof
            // in the redaction's `reason`. When a proof is present it must
            // verify; redactions without one (e.g. manual moderation from a
            // Matrix client) remain governed by the homeserver's
            // authorisation.
            if let Some(proof) = &event.proof
                && !verify_delete_proof(
                    proof,
                    &target_event_id,
                    &c.site_id,
                    &c.post_slug,
                    c.author.public_key.as_deref(),
                )
            {
                warn!(
                    "Rejecting redaction for {} from {}: invalid Cumments delete proof",
                    target_event_id, event.event_id
                );
                return Ok(());
            }
        }

        if self.comment_store.delete_comment(&target_event_id).await? {
            // Keep a persistent tombstone so a later re-delivery of the
            // original event (push retry, resumed backfill) cannot insert it
            // again.
            self.comment_store
                .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                .await?;
            info!("Successfully deleted redacted comment {}", target_event_id);
            // Closed-loop only after the projection succeeded; a failed
            // delete leaves the intent open for the timeout safety net.
            self.intent_store
                .mark_delete_intent_completed(&target_event_id)
                .await?;
            if let Some(c) = comment {
                let _ = self.event_bus.send(ProjectorEvent::CommentDeleted {
                    site_id: c.site_id,
                    post_slug: c.post_slug,
                    event_id: target_event_id,
                });
            }
        } else {
            // The target is unknown (not yet projected, or already deleted).
            // Persist the tombstone so the target cannot resurrect when it is
            // fetched by a later backfill run.
            self.comment_store
                .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                .await?;
            debug!(
                "Redaction tombstoned for unknown target {}",
                target_event_id
            );
        }
        Ok(())
    }

    /// Process a space child state event (room added/removed from a Space).
    #[instrument(skip(self))]
    pub async fn process_space_child(&self, event: ParsedSpaceChild) -> Result<()> {
        let site_id = match event.site_id {
            Some(ref id) => id.clone(),
            None => return Ok(()), // Not a managed Space
        };
        let Ok(site_id_val) = SiteId::new(site_id.clone()) else {
            warn!("Ignoring space child for invalid site id {}", site_id);
            return Ok(());
        };

        // AUTO-DISCOVERY: Ensure the site itself exists in the store
        self.site_store
            .ensure_site_exists(site_id_val.as_str(), &event.space_room_id)
            .await?;

        if event.is_attached {
            // Register the child room if we know its identity
            if let Some(ref child_identity) = event.child_room_identity {
                match PostSlug::new(child_identity.post_slug.clone()) {
                    Ok(post_slug) => {
                        self.registry_store
                            .register_room(&event.child_room_id, &site_id_val, &post_slug)
                            .await?;
                        info!(
                            "Registered active room {} for site {}",
                            event.child_room_id, site_id
                        );
                    }
                    Err(_) => warn!(
                        "Ignoring space child with invalid post slug {}",
                        child_identity.post_slug
                    ),
                }
            }
        } else {
            // Space membership is organizational only: a comment room's
            // identity comes from its metadata + canonical alias, not from
            // being linked into a Space, so unlinking does not deactivate it.
            info!(
                "Room {} unlinked from site space {}; registry unchanged",
                event.child_room_id, event.space_room_id
            );
        }
        Ok(())
    }
}

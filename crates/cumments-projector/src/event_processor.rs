//! Pure event processing logic – independent of how events are received.
//!
//! This module defines the core "projection" functions that transform
//! Matrix events into local read-model updates. It does **not** depend
//! on `matrix_sdk` or any transport-specific types. The AppService
//! `PushReceiver` (and any future transport) calls into these same
//! functions.

use crate::parsed::{
    ParsedPollVote, ParsedReaction, ParsedRoomMessage, ParsedRoomRedaction, ParsedRoomState,
    ParsedSpaceChild,
};
use crate::verification::{verify_delete_proof, verify_guest_event};
use anyhow::Result;
use cumments_core::{
    governance::{
        MODERATOR_LEVEL, POWER_LEVELS_EVENT_TYPE, RoleEntry, is_as_managed_user, role_entries,
    },
    identity::{post_signature_message, signature_message},
    models::{
        AuthorKind, AuthorSnapshot, Content, Message, MessageRevision, MessageStatus, PollVote,
        PostSlug, Reaction, RoomIdentity, RoomMember, RoomStateEvent, RoomStatus, SiteId,
        TextStyle,
    },
    ports::{
        GovernanceStore, MatrixDriver, MessageStore, RegistryStore, RoleClaimStore, RoomStore,
        SiteStore, SubmissionStore,
    },
    projector_events::ProjectorEvent,
    protocol::CLAIM_MESSAGE_PREFIX,
    site_auth::{constant_time_eq, sha256_hex},
};
use std::sync::Arc;
use tokio::sync::{Notify, broadcast};
use tracing::{debug, info, instrument, warn};

// ── Core processing functions ─────────────────────────────────────

/// The central processor – holds only abstract store references.
pub struct EventProcessor {
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
    message_store: Arc<dyn MessageStore>,
    room_store: Arc<dyn RoomStore>,
    governance_store: Arc<dyn GovernanceStore>,
    role_claim_store: Arc<dyn RoleClaimStore>,
    submission_store: Arc<dyn SubmissionStore>,
    driver: Option<Arc<dyn MatrixDriver>>,
    event_bus: broadcast::Sender<ProjectorEvent>,
    /// Wakes the reconciler after a site Space's power levels are projected,
    /// so client-side governance edits propagate to rooms without waiting for
    /// the periodic reconcile tick.
    projection_notify: Arc<Notify>,
    server_name: Option<String>,
}

/// Dependencies of the [`EventProcessor`], kept as one struct so the growing
/// set of stores stays readable at construction sites.
pub struct EventProcessorDeps {
    pub site_store: Arc<dyn SiteStore>,
    pub registry_store: Arc<dyn RegistryStore>,
    pub message_store: Arc<dyn MessageStore>,
    pub room_store: Arc<dyn RoomStore>,
    pub governance_store: Arc<dyn GovernanceStore>,
    pub role_claim_store: Arc<dyn RoleClaimStore>,
    pub submission_store: Arc<dyn SubmissionStore>,
    pub driver: Option<Arc<dyn MatrixDriver>>,
    pub event_bus: broadcast::Sender<ProjectorEvent>,
    pub projection_notify: Arc<Notify>,
    pub server_name: Option<String>,
}

impl EventProcessor {
    pub fn new(deps: EventProcessorDeps) -> Self {
        Self {
            site_store: deps.site_store,
            registry_store: deps.registry_store,
            message_store: deps.message_store,
            room_store: deps.room_store,
            governance_store: deps.governance_store,
            role_claim_store: deps.role_claim_store,
            submission_store: deps.submission_store,
            driver: deps.driver,
            event_bus: deps.event_bus,
            projection_notify: deps.projection_notify,
            server_name: deps.server_name,
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

    /// Attempts to match a plain-text message against a pending token-DM role
    /// claim. Returns `true` when the message activated a claim and therefore
    /// must not be projected as a comment.
    pub async fn process_claim_dm(&self, event: &ParsedRoomMessage) -> Result<bool> {
        if event.is_virtual_user_sender {
            return Ok(false);
        }
        let Content::Text(text) = &event.content else {
            return Ok(false);
        };
        if text.style != TextStyle::Normal
            || event.relates_to.is_some()
            || event.reply_to.is_some()
            || event.thread_root.is_some()
        {
            return Ok(false);
        }
        let Some(token) = text.body.trim().strip_prefix(CLAIM_MESSAGE_PREFIX) else {
            return Ok(false);
        };
        let token = token.trim();
        if token.is_empty() {
            return Ok(false);
        }

        let presented_hash = sha256_hex(token.as_bytes());
        for claim in self
            .role_claim_store
            .pending_claims_for_user(&event.sender)
            .await?
        {
            if constant_time_eq(claim.token_hash.as_bytes(), presented_hash.as_bytes())
                && self.role_claim_store.mark_claim_activated(claim.id).await?
            {
                info!("Activated role claim {} for {}", claim.id, claim.user_id);
                self.projection_notify.notify_one();
                return Ok(true);
            }
        }
        Ok(false)
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
        let registry_status = self.registry_store.get_room_status(&event.room_id).await?;

        match registry_status {
            Some(RoomStatus::Active) => {
                // Room is active, proceed normally
            }
            Some(_) => {
                // Room is quarantined, superseded or otherwise not canonical.
                debug!("Ignoring message from non-active room {}", event.room_id);
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
            .message_store
            .has_backfill_tombstone(&event.event_id, &event.room_id)
            .await?
        {
            debug!("Ignoring tombstoned event {}", event.event_id);
            return Ok(());
        }

        // Handle Edits (Replacements)
        if let Some(ref relation) = event.relates_to {
            info!("Handling edit for event {}", relation.target_event_id);

            let existing = self
                .message_store
                .get_message(&relation.target_event_id)
                .await?;

            // Integrity: Matrix does not enforce same-sender on m.replace, so
            // verify the replacement was sent by the original message's author
            // virtual user. Legacy rows without a recorded sender are accepted
            // until re-projected by backfill.
            if let Some(ref existing) = existing
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
                        relation.signable_content().is_some_and(|new_content| {
                            let message = signature_message(&[
                                "PATCH",
                                &site_id,
                                &post_slug,
                                &relation.target_event_id,
                                new_content,
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
                        })
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

            let edited_at = chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
                .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
            let Some(mut updated) = existing else {
                debug!(
                    "Edit ignored for {}: target missing",
                    relation.target_event_id
                );
                return Ok(());
            };
            updated.content = relation.new_content.clone();
            updated.edited_at = Some(edited_at);
            let revision = MessageRevision {
                event_id: event.event_id.clone(),
                content: updated.content.clone(),
                edited_at,
                editor_mxid: event.sender.clone(),
            };

            if self.message_store.apply_edit(&updated, &revision).await? {
                info!("Successfully updated message {}", relation.target_event_id);
                // Closed-loop only after the projection succeeded: the
                // correlation ID lets concurrent edits close independently;
                // legacy events fall back to target-event matching (waiting
                // submissions only). A failed projection leaves the submission open
                // for the timeout/backfill safety net.
                match event.submission_id {
                    Some(id) => {
                        self.submission_store
                            .mark_update_submission_completed_by_id(id)
                            .await?
                    }
                    None => {
                        self.submission_store
                            .mark_update_submission_completed(
                                &relation.target_event_id,
                                event.author_public_key.as_deref(),
                            )
                            .await?
                    }
                };
                if let Some(message) = self
                    .message_store
                    .get_message(&relation.target_event_id)
                    .await?
                {
                    let _ = self.event_bus.send(ProjectorEvent::MessageUpdated {
                        site_id,
                        post_slug,
                        message,
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
                    let message = match &event.content {
                        // Locations use the standalone LOCATE signature;
                        // text/media use the POST signature (which binds the
                        // display name and reply relation).
                        Content::Location(location) => signature_message(&[
                            "LOCATE",
                            &site_id,
                            &post_slug,
                            &location.geo_uri,
                            nick,
                            chal,
                        ]),
                        _ => match event.signable_content() {
                            Some(content) => post_signature_message(
                                &site_id,
                                &post_slug,
                                content,
                                nick,
                                event.reply_to.as_deref(),
                                chal,
                            ),
                            None => return Ok(()),
                        },
                    };
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

        // Handle original messages.
        let is_matrix_native = !event.is_virtual_user_sender;
        let message = Message {
            event_id: event.event_id.clone(),
            site_id: site_id.clone(),
            post_slug: post_slug.clone(),
            author: AuthorSnapshot {
                kind: if is_matrix_native {
                    AuthorKind::Matrix
                } else {
                    AuthorKind::Guest
                },
                display_name: event.display_name.clone(),
                avatar_url: None,
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
            edited_at: None,
            reply_to: event.reply_to.clone(),
            thread_root: event.thread_root.clone(),
            submission_id: event.submission_id,
            status: MessageStatus::Active,
            redacted_at: None,
            redacted_by: None,
            reactions: Vec::new(),
            room_id: event.room_id.clone(),
            sender_mxid: event.sender.clone(),
            raw_content: event.raw_content.clone(),
        };

        self.message_store.save_message(&message).await?;
        info!("Successfully projected message event {}", event.event_id);
        // Closed-loop only after the projection succeeded. Prefer the
        // correlation ID when present – the push may arrive before the
        // reconciler's write-back, so the event_id is not yet stored on the
        // submission row. Fall back to event_id matching for external messages.
        match event.submission_id {
            Some(id) => {
                self.submission_store
                    .mark_post_submission_completed_by_id(id)
                    .await?
            }
            None => {
                self.submission_store
                    .mark_post_submission_completed(&event.event_id)
                    .await?
            }
        };
        let _ = self.event_bus.send(ProjectorEvent::MessageCreated {
            site_id,
            post_slug,
            message,
        });
        Ok(())
    }

    /// Process a reaction event (m.reaction) into the annotations store.
    #[instrument(skip(self))]
    pub async fn process_reaction(&self, event: ParsedReaction) -> Result<()> {
        match self.registry_store.get_room_status(&event.room_id).await? {
            Some(RoomStatus::Active) => {}
            Some(_) => {
                debug!("Ignoring reaction from non-active room {}", event.room_id);
                return Ok(());
            }
            None => {
                debug!("Ignoring reaction from unregistered room {}", event.room_id);
                return Ok(());
            }
        }
        // A redaction may have been seen before its target (capped/resumed
        // backfill, push retry); never re-project a tombstoned reaction.
        if self
            .message_store
            .has_backfill_tombstone(&event.event_id, &event.room_id)
            .await?
        {
            debug!("Ignoring tombstoned reaction {}", event.event_id);
            return Ok(());
        }
        // Guest reactions carry a signed proof block that must verify.
        if event.is_virtual_user_sender {
            let (Some(pk), Some(sig), Some(chal)) = (
                event.author_public_key.as_deref(),
                event.author_signature.as_deref(),
                event.author_challenge.as_deref(),
            ) else {
                warn!(
                    "Rejecting guest reaction {} from {}: missing proof block",
                    event.event_id, event.sender
                );
                return Ok(());
            };
            let Some(identity) = &event.room_identity else {
                debug!("Ignoring reaction {} without room identity", event.event_id);
                return Ok(());
            };
            let message = signature_message(&[
                "REACT",
                &identity.site_id,
                &identity.post_slug,
                &event.message_event_id,
                &event.key,
                chal,
            ]);
            if !verify_guest_event(
                self.server_name.as_deref(),
                &event.sender,
                &identity.site_id,
                pk,
                sig,
                &message,
            ) {
                warn!(
                    "Rejecting guest reaction {} from {}: invalid proof",
                    event.event_id, event.sender
                );
                return Ok(());
            }
        }
        // The reaction must target a projected message in the same room.
        let Some(target) = self
            .message_store
            .get_message(&event.message_event_id)
            .await?
        else {
            debug!(
                "Ignoring reaction {} for unknown message {}",
                event.event_id, event.message_event_id
            );
            return Ok(());
        };
        if target.room_id != event.room_id {
            warn!(
                "Ignoring reaction {} for {}: message lives in {}",
                event.event_id, event.message_event_id, target.room_id
            );
            return Ok(());
        }
        let message_event_id = event.message_event_id.clone();
        self.message_store
            .save_reaction(&Reaction {
                event_id: event.event_id,
                message_event_id: message_event_id.clone(),
                sender_mxid: event.sender,
                key: event.key,
                origin_server_ts: event.origin_server_ts,
                redacted_at: None,
            })
            .await?;
        if let Some(updated) = self.message_store.get_message(&message_event_id).await? {
            let _ = self
                .event_bus
                .send(ProjectorEvent::MessageAnnotationsChanged {
                    site_id: updated.site_id.clone(),
                    post_slug: updated.post_slug.clone(),
                    message: updated,
                });
        }
        Ok(())
    }

    /// Process a poll response by mapping answer IDs to option indexes on the
    /// stored poll, then recording the vote.
    #[instrument(skip(self))]
    pub async fn process_poll_vote(&self, event: ParsedPollVote) -> Result<()> {
        match self.registry_store.get_room_status(&event.room_id).await? {
            Some(RoomStatus::Active) => {}
            Some(_) => {
                debug!("Ignoring poll vote from non-active room {}", event.room_id);
                return Ok(());
            }
            None => {
                debug!(
                    "Ignoring poll vote from unregistered room {}",
                    event.room_id
                );
                return Ok(());
            }
        }
        // Same tombstone gate as messages/reactions: a redaction seen before
        // the original vote must prevent resurrection on re-delivery.
        if self
            .message_store
            .has_backfill_tombstone(&event.event_id, &event.room_id)
            .await?
        {
            debug!("Ignoring tombstoned poll vote {}", event.event_id);
            return Ok(());
        }

        let Some(answer_id) = event.answer_ids.first() else {
            debug!("Poll vote without answers; ignoring");
            return Ok(());
        };

        if event.is_virtual_user_sender {
            let (Some(pk), Some(sig), Some(chal)) = (
                event.author_public_key.as_deref(),
                event.author_signature.as_deref(),
                event.author_challenge.as_deref(),
            ) else {
                warn!(
                    "Rejecting guest vote {} from {}: missing proof block",
                    event.event_id, event.sender
                );
                return Ok(());
            };
            let Some(identity) = &event.room_identity else {
                debug!(
                    "Ignoring poll vote {} without room identity",
                    event.event_id
                );
                return Ok(());
            };
            let message = signature_message(&[
                "VOTE",
                &identity.site_id,
                &identity.post_slug,
                &event.poll_message_id,
                answer_id,
                chal,
            ]);
            if !verify_guest_event(
                self.server_name.as_deref(),
                &event.sender,
                &identity.site_id,
                pk,
                sig,
                &message,
            ) {
                warn!(
                    "Rejecting guest vote {} from {}: invalid proof",
                    event.event_id, event.sender
                );
                return Ok(());
            }
        }

        let Some(message) = self
            .message_store
            .get_message(&event.poll_message_id)
            .await?
        else {
            debug!(
                "Poll vote for unknown poll {}; ignoring",
                event.poll_message_id
            );
            return Ok(());
        };
        let Content::Poll(poll) = &message.content else {
            debug!(
                "Poll vote target {} is not a poll; ignoring",
                event.poll_message_id
            );
            return Ok(());
        };
        let Some(option_index) = poll.options.iter().position(|o| &o.id == answer_id) else {
            debug!("Poll vote references unknown answer {answer_id}; ignoring");
            return Ok(());
        };
        self.message_store
            .save_poll_vote(&PollVote {
                event_id: event.event_id,
                poll_message_id: event.poll_message_id,
                sender_mxid: event.sender,
                option_index: option_index as i64,
                origin_server_ts: event.origin_server_ts,
            })
            .await?;
        if let Some(updated) = self.message_store.get_message(&message.event_id).await? {
            let _ = self
                .event_bus
                .send(ProjectorEvent::MessageAnnotationsChanged {
                    site_id: updated.site_id.clone(),
                    post_slug: updated.post_slug.clone(),
                    message: updated,
                });
        }
        Ok(())
    }

    /// Process a room state event (system message / room metadata).
    #[instrument(skip(self))]
    pub async fn process_room_state(&self, event: ParsedRoomState) -> Result<()> {
        if event.event_type == "m.room.member" {
            let membership = event
                .content
                .get("membership")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Conditional auto-join: accept claim-DM invites only when the
            // inviter has a pending role claim. Unconditional auto-join would
            // let anyone pull the bot into arbitrary rooms, after which the
            // homeserver pushes that room's whole event stream to the AS.
            if membership == "invite"
                && self
                    .driver
                    .as_ref()
                    .and_then(|driver| driver.sender_user_id())
                    .as_deref()
                    == Some(event.state_key.as_str())
            {
                let inviter = &event.sender;
                let claims = self
                    .role_claim_store
                    .pending_claims_for_user(inviter)
                    .await?;
                if claims.is_empty() {
                    debug!(
                        "Ignoring invite for {} in {}: no pending role claim",
                        inviter, event.room_id
                    );
                } else if let Some(driver) = &self.driver {
                    match driver.join_room(&event.room_id).await {
                        Ok(()) => {
                            self.role_claim_store
                                .set_claim_dm_room_for_user(inviter, &event.room_id)
                                .await?;
                            info!("Bot joined claim DM {} for {}", event.room_id, inviter);
                        }
                        Err(e) => warn!(
                            "Bot failed to join claim DM {} for {}: {:#}",
                            event.room_id, inviter, e
                        ),
                    }
                }
            }
            self.room_store
                .save_member(&RoomMember {
                    room_id: event.room_id.clone(),
                    // `m.room.member` state key is the member's user ID.
                    user_id: event.state_key.clone(),
                    display_name: event
                        .content
                        .get("displayname")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    avatar_url: event
                        .content
                        .get("avatar_url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    membership,
                    updated_at: chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
                        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
                })
                .await?;
        }
        if event.event_type == "m.room.encryption"
            && self
                .role_claim_store
                .claim_dm_room_exists(&event.room_id)
                .await?
        {
            warn!(
                "Claim DM {} is encrypted; verification tokens cannot be read \
                 (claim DMs must be unencrypted)",
                event.room_id
            );
        }
        if event.event_type == POWER_LEVELS_EVENT_TYPE {
            let roles: Vec<RoleEntry> = role_entries(&event.content, MODERATOR_LEVEL)
                .into_iter()
                .filter(|role| !is_as_managed_user(&role.user_id))
                .collect();
            // A site Space's power levels define the site roles; a comment
            // room's power levels define its room roles. Other rooms (or
            // unregistered rooms) carry no governance meaning.
            if let Some(site) = self.site_store.get_site_by_space_id(&event.room_id).await? {
                self.governance_store
                    .replace_site_roles(&site.id, &roles)
                    .await?;
                self.projection_notify.notify_one();
            } else if matches!(
                self.registry_store.get_room_status(&event.room_id).await?,
                Some(RoomStatus::Active)
            ) {
                self.governance_store
                    .replace_room_roles(&event.room_id, &roles)
                    .await?;
            }
        }

        self.room_store
            .save_state_event(&RoomStateEvent {
                event_id: event.event_id,
                room_id: event.room_id,
                event_type: event.event_type,
                state_key: event.state_key,
                sender: event.sender,
                origin_server_ts: event.origin_server_ts,
                content_json: event.content,
            })
            .await?;
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
        match self.registry_store.get_room_status(&event.room_id).await? {
            Some(RoomStatus::Active) => {}
            Some(_) => {
                debug!("Ignoring redaction from non-active room {}", event.room_id);
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

        // Integrity: only redact a target that actually lives in the room the
        // redaction arrived from. Fetch before redacting so the check uses
        // the same snapshot the deletion will operate on.
        let redacted_at = chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
        let redacted_by = event
            .sender
            .clone()
            .unwrap_or_else(|| event.event_id.clone());

        // 1. Comment message targets.
        if let Some(c) = self.message_store.get_message(&target_event_id).await? {
            if c.room_id != event.room_id {
                warn!(
                    "Ignoring redaction for {} in {}: message lives in {}",
                    target_event_id, event.room_id, c.room_id
                );
                return Ok(());
            }
            if let Some(ref identity) = event.room_identity
                && (c.site_id != identity.site_id || c.post_slug != identity.post_slug)
            {
                warn!(
                    "Ignoring redaction for {} in {}: message belongs to {}/{}",
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

            if self
                .message_store
                .redact_message(&target_event_id, &event.room_id, redacted_at, &redacted_by)
                .await?
            {
                // Keep a persistent tombstone so a later re-delivery of the
                // original event (push retry, resumed backfill) cannot insert
                // it again.
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                info!("Successfully redacted message {}", target_event_id);
                // Closed-loop only after the projection succeeded; a failed
                // delete leaves the submission open for the timeout safety net.
                self.submission_store
                    .mark_delete_submission_completed(&target_event_id)
                    .await?;
                let _ = self.event_bus.send(ProjectorEvent::MessageDeleted {
                    site_id: c.site_id,
                    post_slug: c.post_slug,
                    event_id: target_event_id,
                    submission_id: event.submission_id,
                });
            } else {
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
            }
            return Ok(());
        }

        // 2. Reaction targets: verify the annotated message lives in this
        // room, then redact the reaction row so it leaves the aggregate.
        if let Some(reaction) = self.message_store.get_reaction(&target_event_id).await? {
            let Some(target) = self
                .message_store
                .get_message(&reaction.message_event_id)
                .await?
            else {
                debug!(
                    "Redaction tombstoned for reaction {}: target message unknown",
                    target_event_id
                );
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                return Ok(());
            };
            if target.room_id != event.room_id {
                warn!(
                    "Ignoring redaction for {} in {}: reaction lives in {}",
                    target_event_id, event.room_id, target.room_id
                );
                return Ok(());
            }
            if self
                .message_store
                .redact_reaction(&target_event_id, redacted_at)
                .await?
            {
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                info!("Successfully redacted reaction {}", target_event_id);
                if let Some(updated) = self
                    .message_store
                    .get_message(&reaction.message_event_id)
                    .await?
                {
                    let _ = self
                        .event_bus
                        .send(ProjectorEvent::MessageAnnotationsChanged {
                            site_id: updated.site_id.clone(),
                            post_slug: updated.post_slug.clone(),
                            message: updated,
                        });
                }
            }
            return Ok(());
        }

        // 3. Poll-vote targets (same room check through the poll message).
        if let Some(vote) = self
            .message_store
            .get_poll_vote_by_event(&target_event_id)
            .await?
        {
            let Some(target) = self
                .message_store
                .get_message(&vote.poll_message_id)
                .await?
            else {
                debug!(
                    "Redaction tombstoned for poll vote {}: poll message unknown",
                    target_event_id
                );
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                return Ok(());
            };
            if target.room_id != event.room_id {
                warn!(
                    "Ignoring redaction for {} in {}: vote lives in {}",
                    target_event_id, event.room_id, target.room_id
                );
                return Ok(());
            }
            if self
                .message_store
                .redact_poll_vote(&target_event_id, redacted_at, &redacted_by)
                .await?
            {
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                info!("Successfully redacted poll vote {}", target_event_id);
                if let Some(updated) = self
                    .message_store
                    .get_message(&vote.poll_message_id)
                    .await?
                {
                    let _ = self
                        .event_bus
                        .send(ProjectorEvent::MessageAnnotationsChanged {
                            site_id: updated.site_id.clone(),
                            post_slug: updated.post_slug.clone(),
                            message: updated,
                        });
                }
            }
            return Ok(());
        }

        // 4. Unknown target: persist the tombstone so the target cannot
        // resurrect when it is fetched by a later backfill run.
        self.message_store
            .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
            .await?;
        debug!(
            "Redaction tombstoned for unknown target {}",
            target_event_id
        );
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

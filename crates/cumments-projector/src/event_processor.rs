//! Pure event processing logic – independent of how events are received.
//!
//! This module defines the core "projection" functions that transform
//! Matrix events into local read-model updates. It does **not** depend
//! on `matrix_sdk` or any transport-specific types. The AppService
//! `PushReceiver` (and any future transport) calls into these same
//! functions.

use anyhow::Result;
use cumments_core::{
    identity::{
        derive_guest_id_from_public_key, post_signature_message, signature_message,
        verify_signature,
    },
    models::{AuthorType, Comment, CommentAuthor, PostSlug, SiteId},
    ports::{CommentStore, IntentStore, RegistryStore, SiteStore},
    projector_events::ProjectorEvent,
    protocol::REDACTION_PROOF_KEY,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, instrument, warn};

// ── Pure data structures (transport-agnostic) ──────────────────────

/// Identity of a Cumments room, extracted from metadata or alias.
#[derive(Debug, Clone)]
pub struct RoomIdentity {
    pub site_id: String,
    pub post_slug: String,
}

/// A parsed room message event.
#[derive(Debug)]
pub struct ParsedRoomMessage {
    pub room_id: String,
    pub event_id: String,
    /// The Matrix user ID of the sender.
    pub sender: String,
    /// The plain-text body of the message.
    pub content: String,
    /// The resolved display name of the author, if available.
    pub displayname: Option<String>,
    /// The author's Ed25519 public key embedded in the event, if any.
    pub author_public_key: Option<String>,
    /// The author's Ed25519 signature embedded in the event, if any.
    pub author_signature: Option<String>,
    /// The PoW challenge prefix embedded in the event, if any.
    pub author_challenge: Option<String>,
    /// Whether the sender is one of our exclusive AS virtual users.
    pub is_virtual_user_sender: bool,
    /// Correlation hint: the intent queue row ID that produced this event,
    /// if the message was sent by Cumments.
    pub intent_id: Option<i64>,
    /// Matrix event ID of the parent comment, if this event is a rich reply.
    pub reply_to: Option<String>,
    pub origin_server_ts: i64,
    /// If this is an edit (m.replace), the relation details.
    pub relates_to: Option<ParsedRelation>,
    /// The room's Cumments identity, if it could be resolved.
    pub room_identity: Option<RoomIdentity>,
}

/// A parsed relation (edit) attached to a message.
#[derive(Debug)]
pub struct ParsedRelation {
    pub target_event_id: String,
    pub new_content: String,
}

/// A parsed redaction event.
#[derive(Debug)]
pub struct ParsedRoomRedaction {
    pub room_id: String,
    pub event_id: String,
    /// The event ID being redacted (may be in `redacts` top-level or `.content.redacts`).
    pub redacts: Option<String>,
    /// The Cumments delete proof embedded in `reason`, if the redaction was
    /// issued through the Cumments API.
    pub proof: Option<serde_json::Value>,
    /// The room's Cumments identity, if available.
    pub room_identity: Option<RoomIdentity>,
}

/// A parsed space-child state event (room added/removed from a Space).
#[derive(Debug)]
pub struct ParsedSpaceChild {
    pub space_room_id: String,
    /// The site_id resolved from the Space's own metadata.
    pub site_id: Option<String>,
    pub child_room_id: String,
    /// `true` if the child is being attached, `false` if removed.
    pub is_attached: bool,
    /// The child room's Cumments identity, if it could be resolved.
    pub child_room_identity: Option<RoomIdentity>,
}

// ── Metadata helpers ──────────────────────────────────────────────

/// Internal helper for deserialising Cumments room metadata.
#[derive(Deserialize)]
struct RoomMetadata {
    site_id: String,
    post_slug: Option<String>,
}

/// Resolve a `RoomIdentity` from optional metadata JSON and optional
/// canonical alias, using the same two-phase strategy as the original
/// `get_room_identity`:  (1) metadata state event, (2) alias fallback.
///
/// This is a **pure** function – no I/O, no SDK dependency.
pub fn parse_room_identity(
    metadata_json: Option<&str>,
    canonical_alias: Option<&str>,
) -> Option<RoomIdentity> {
    // Phase 1 – Try metadata first (source of truth)
    if let Some(json) = metadata_json
        && let Ok(m) = serde_json::from_str::<RoomMetadata>(json)
        && let Some(slug) = m.post_slug
    {
        return Some(RoomIdentity {
            site_id: m.site_id,
            post_slug: slug,
        });
    }

    // Phase 2 – Fallback to alias parsing for legacy rooms
    let alias = canonical_alias?;
    let alias_str = alias;

    // Supports #_cumments_SITE_ID_POST_SLUG:domain.
    let localpart = alias_str.split(':').next()?.strip_prefix('#')?;
    let content_part = localpart.strip_prefix("_cumments_")?;
    let parts: Vec<_> = content_part.splitn(2, '_').collect();

    if parts.len() == 2 {
        Some(RoomIdentity {
            site_id: parts[0].to_string(),
            post_slug: parts[1].to_string(),
        })
    } else {
        None
    }
}

/// Verify a guest event's identity claims.
///
/// The sender must be exactly the virtual user derived from the embedded
/// public key for this site, and the Ed25519 signature must cover the
/// canonical Cumments message. Matrix-native senders never pass through this
/// path; `server_name` is required in AppService mode.
fn verify_guest_event(
    server_name: Option<&str>,
    sender: &str,
    site_id: &str,
    public_key: &str,
    signature: &str,
    message: &str,
) -> bool {
    let Some(guest_id) = derive_guest_id_from_public_key(public_key) else {
        return false;
    };
    let Some(server_name) = server_name else {
        return false;
    };
    let expected_sender = format!("@_cumments_{}_{}:{}", site_id, guest_id, server_name);
    if sender != expected_sender {
        return false;
    }
    verify_signature(public_key, message, signature)
}

/// Verify a Cumments delete proof embedded in a redaction's `reason`.
///
/// The proof is the JSON object the API stores under
/// `host.curious.cumments` when a guest requests deletion: site/post/target
/// must match the comment being redacted and the Ed25519 signature must cover
/// the canonical DELETE message. Returns `false` for missing or malformed
/// proofs so callers can reject the redaction.
fn verify_delete_proof(
    proof: &serde_json::Value,
    target_event_id: &str,
    site_id: &str,
    post_slug: &str,
    author_public_key: Option<&str>,
) -> bool {
    let Some(block) = proof.get(REDACTION_PROOF_KEY) else {
        return false;
    };
    let field = |key: &str| block.get(key).and_then(|v| v.as_str());
    let (
        Some(proof_site),
        Some(proof_slug),
        Some(proof_target),
        Some(public_key),
        Some(signature),
        Some(challenge),
    ) = (
        field("site_id"),
        field("post_slug"),
        field("target_event_id"),
        field("public_key"),
        field("signature"),
        field("challenge"),
    )
    else {
        return false;
    };

    if proof_site != site_id || proof_slug != post_slug || proof_target != target_event_id {
        return false;
    }
    if author_public_key.is_some_and(|key| key != public_key) {
        return false;
    }

    let message = signature_message(&["DELETE", site_id, post_slug, target_event_id, challenge]);
    verify_signature(public_key, &message, signature)
}

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
            .map(|identity| {
                identity.map(|(site_id, post_slug)| RoomIdentity { site_id, post_slug })
            })
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
                .update_comment_content(&relation.target_event_id, &relation.new_content)
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
                    "Edit received for unknown comment {}",
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
                &event.displayname,
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
                displayname: event.displayname.clone(),
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
            debug!(
                "Redaction received for unknown or already deleted comment {}",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_room_identity_from_preferred_underscored_alias() {
        let identity =
            parse_room_identity(None, Some("#_cumments_my-blog_hello-world:example.com"));

        assert!(matches!(
            identity,
            Some(RoomIdentity {
                site_id,
                post_slug
            }) if site_id == "my-blog" && post_slug == "hello-world"
        ));
    }

    #[test]
    fn parse_room_identity_prefers_metadata_over_alias() {
        let metadata = r#"{"site_id": "meta-site", "post_slug": "meta-post"}"#;
        let identity = parse_room_identity(
            Some(metadata),
            Some("#_cumments_alias-site_alias-post:example.com"),
        );

        assert!(matches!(
            identity,
            Some(RoomIdentity {
                site_id,
                post_slug
            }) if site_id == "meta-site" && post_slug == "meta-post"
        ));
    }

    #[test]
    fn guest_event_verification_accepts_only_expected_virtual_sender() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use cumments_core::identity::post_signature_message;
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let guest_id = derive_guest_id_from_public_key(&public_key).expect("guest id");
        let sender = format!("@_cumments_my-blog_{}:example.com", guest_id);
        let challenge = "challenge";
        let message =
            post_signature_message("my-blog", "hello", "content", "Alice", None, challenge);
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());

        assert!(verify_guest_event(
            Some("example.com"),
            &sender,
            "my-blog",
            &public_key,
            &signature,
            &message,
        ));
        // Wrong server name must fail the sender check.
        assert!(!verify_guest_event(
            Some("other.example.com"),
            &sender,
            "my-blog",
            &public_key,
            &signature,
            &message,
        ));
        // A sender that does not match the derived virtual user must fail.
        assert!(!verify_guest_event(
            Some("example.com"),
            "@_cumments_my-blog_0000000000000000:example.com",
            "my-blog",
            &public_key,
            &signature,
            &message,
        ));
        // Without a configured server name there is nothing to bind to.
        assert!(!verify_guest_event(
            None,
            &sender,
            "my-blog",
            &public_key,
            &signature,
            &message,
        ));
        // Tampered signature must fail.
        assert!(!verify_guest_event(
            Some("example.com"),
            &sender,
            "my-blog",
            &public_key,
            "AAAA",
            &message,
        ));
    }

    #[test]
    fn delete_proof_verifies_valid_signature_and_rejects_tampering() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[11u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let challenge = "challenge";
        let message = signature_message(&["DELETE", "my-blog", "hello", "$target:hs", challenge]);
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());

        let proof = serde_json::json!({
            "host.curious.cumments.redaction": {
                "site_id": "my-blog",
                "post_slug": "hello",
                "target_event_id": "$target:hs",
                "public_key": public_key,
                "signature": signature,
                "challenge": challenge,
            }
        });

        assert!(verify_delete_proof(
            &proof,
            "$target:hs",
            "my-blog",
            "hello",
            Some(&public_key),
        ));
        // Wrong target, site, or stored author key must fail.
        assert!(!verify_delete_proof(
            &proof,
            "$other:hs",
            "my-blog",
            "hello",
            Some(&public_key),
        ));
        assert!(!verify_delete_proof(
            &proof,
            "$target:hs",
            "other",
            "hello",
            Some(&public_key),
        ));
        assert!(!verify_delete_proof(
            &proof,
            "$target:hs",
            "my-blog",
            "hello",
            Some("some-other-key"),
        ));
        // Missing or malformed proofs are rejected.
        assert!(!verify_delete_proof(
            &serde_json::json!({}),
            "$target:hs",
            "my-blog",
            "hello",
            Some(&public_key),
        ));
        assert!(!verify_delete_proof(
            &serde_json::json!({ "host.curious.cumments.redaction": { "site_id": "my-blog" } }),
            "$target:hs",
            "my-blog",
            "hello",
            Some(&public_key),
        ));
    }
}

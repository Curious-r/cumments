//! Pure event processing logic – independent of how events are received.
//!
//! This module defines the core "projection" functions that transform
//! Matrix events into local read-model updates. It does **not** depend
//! on `matrix_sdk` or any transport-specific types. The AppService
//! `PushReceiver` (and any future transport) calls into these same
//! functions.

use cumments_core::{
    events::ProjectorEvent,
    models::Comment,
    ports::{CommentStore, IntentStore, RegistryStore, SiteStore},
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
    pub author_display_name: Option<String>,
    /// The Cumments fingerprint embedded in the event, if any.
    pub fingerprint: Option<String>,
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

/// Extract just the site_id from a Cumments room's (or Space's) metadata JSON.
/// Unlike `parse_room_identity`, this works for Spaces where `post_slug` is None.
pub fn parse_site_id_from_metadata(metadata_json: &str) -> Option<String> {
    serde_json::from_str::<RoomMetadata>(metadata_json)
        .ok()
        .map(|m| m.site_id)
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
    if let Some(json) = metadata_json {
        if let Ok(m) = serde_json::from_str::<RoomMetadata>(json) {
            if let Some(slug) = m.post_slug {
                return Some(RoomIdentity {
                    site_id: m.site_id,
                    post_slug: slug,
                });
            }
        }
    }

    // Phase 2 – Fallback to alias parsing for legacy rooms
    let alias = canonical_alias?;
    let alias_str = alias;

    // Supports #cumments_SITE_ID_POST_SLUG:domain and #SITE_ID_POST_SLUG:domain
    let localpart = alias_str.split(':').next()?.strip_prefix('#')?;
    let content_part = localpart.strip_prefix("cumments_").unwrap_or(localpart);
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

// ── Core processing functions ─────────────────────────────────────

/// The central processor – holds only abstract store references.
pub struct EventProcessor {
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
    comment_store: Arc<dyn CommentStore>,
    intent_store: Arc<dyn IntentStore>,
    event_bus: broadcast::Sender<ProjectorEvent>,
}

impl EventProcessor {
    pub fn new(
        site_store: Arc<dyn SiteStore>,
        registry_store: Arc<dyn RegistryStore>,
        comment_store: Arc<dyn CommentStore>,
        intent_store: Arc<dyn IntentStore>,
        event_bus: broadcast::Sender<ProjectorEvent>,
    ) -> Self {
        Self {
            site_store,
            registry_store,
            comment_store,
            intent_store,
            event_bus,
        }
    }

    /// Look up the site ID associated with a Matrix Space room ID.
    /// Returns `None` if the space is not in our local database.
    pub async fn get_site_id_by_space_id(&self, space_id: &str) -> Option<String> {
        self.site_store
            .get_site_by_space_id(space_id)
            .await
            .ok()
            .flatten()
            .map(|s| s.id)
    }

    /// Process a room message (new comment or edit).
    #[instrument(skip(self))]
    pub async fn process_room_message(&self, event: ParsedRoomMessage) {
        // ── PRINCIPLE B: REGISTRY ENFORCEMENT ──
        let registry_status = self
            .registry_store
            .is_room_active(&event.room_id)
            .await
            .unwrap_or(None);

        match registry_status {
            Some(true) => {
                // Room is active, proceed normally
            }
            Some(false) => {
                // Room is explicitly INACTIVE (tombstoned).
                debug!("Ignoring message from deactivated room {}", event.room_id);
                return;
            }
            None => {
                // Room is completely UNKNOWN to us.
                // Attempt just-in-time registration if we have its identity.
                if let Some(ref identity) = event.room_identity {
                    let _ = self
                        .site_store
                        .ensure_site_exists(&identity.site_id, &event.room_id)
                        .await;

                    let _ = self
                        .registry_store
                        .register_room(
                            &event.room_id,
                            &identity.site_id.clone().into(),
                            &identity.post_slug.clone().into(),
                        )
                        .await;
                    info!(
                        "Just-in-time registered new room {} for site {}",
                        event.room_id, identity.site_id
                    );
                } else {
                    debug!("Ignoring message from unregistered room {}", event.room_id);
                    return;
                }
            }
        }

        // 0. Identify the room context
        let (site_id, post_slug) = match event.room_identity {
            Some(ref id) => (id.site_id.clone(), id.post_slug.clone()),
            None => return, // Not a cumments room
        };

        // 1. Closed-loop: Mark any waiting intent as completed
        if let Err(e) = self
            .intent_store
            .mark_post_intent_completed(&event.event_id)
            .await
        {
            debug!(
                "Failed to mark intent as completed (normal if external msg): {:?}",
                e
            );
        }

        // Handle Edits (Replacements)
        if let Some(ref relation) = event.relates_to {
            info!("Handling edit for event {}", relation.target_event_id);

            match self
                .comment_store
                .update_comment_content(&relation.target_event_id, &relation.new_content)
                .await
            {
                Ok(true) => {
                    info!("Successfully updated comment {}", relation.target_event_id);

                    // Try to fetch updated comment to emit full object
                    if let Ok(Some(comment)) = self
                        .comment_store
                        .get_comment(&relation.target_event_id)
                        .await
                    {
                        let _ = self.event_bus.send(ProjectorEvent::CommentUpdated {
                            site_id,
                            post_slug,
                            comment,
                        });
                    }
                }
                Ok(false) => debug!(
                    "Edit received for unknown comment {}",
                    relation.target_event_id
                ),
                Err(e) => warn!(
                    "Failed to update comment {}: {:?}",
                    relation.target_event_id, e
                ),
            }
            return;
        }

        // Handle Original Posts
        let comment = Comment {
            event_id: event.event_id.clone(),
            site_id: site_id.clone(),
            post_slug: post_slug.clone(),
            author_nickname: event.author_display_name.clone(),
            author_fingerprint: event.fingerprint.clone(),
            content: event.content.clone(),
            timestamp: chrono::DateTime::from_timestamp_millis(event.origin_server_ts).unwrap(),
        };

        match self
            .comment_store
            .save_comment(
                &comment,
                &event.room_id,
                &site_id.clone().into(),
                &post_slug.clone().into(),
            )
            .await
        {
            Ok(_) => {
                info!("Successfully projected comment event {}", event.event_id);
                let _ = self.event_bus.send(ProjectorEvent::NewComment {
                    site_id,
                    post_slug,
                    comment,
                });
            }
            Err(e) => debug!(
                "Failed to project comment event {}: {:?}",
                event.event_id, e
            ),
        }
    }

    /// Process a redaction event (comment deletion).
    #[instrument(skip(self))]
    pub async fn process_room_redaction(&self, event: ParsedRoomRedaction) {
        let target_event_id = match event.redacts {
            Some(ref id) => id.clone(),
            None => {
                debug!("Redaction event without a target event_id, ignoring");
                return;
            }
        };

        // Closed-loop: Mark delete intent as completed
        if let Err(e) = self
            .intent_store
            .mark_delete_intent_completed(&target_event_id)
            .await
        {
            debug!("Failed to mark delete intent as completed: {:?}", e);
        }

        info!(
            "Handling redaction for event {} in room {}",
            target_event_id, event.room_id
        );

        // Fetch comment before deleting to emit event with context
        let comment = self
            .comment_store
            .get_comment(&target_event_id)
            .await
            .ok()
            .flatten();

        match self.comment_store.delete_comment(&target_event_id).await {
            Ok(true) => {
                info!("Successfully deleted redacted comment {}", target_event_id);
                if let Some(c) = comment {
                    let _ = self.event_bus.send(ProjectorEvent::CommentDeleted {
                        site_id: c.site_id,
                        post_slug: c.post_slug,
                        event_id: target_event_id,
                    });
                }
            }
            Ok(false) => debug!(
                "Redaction received for unknown or already deleted comment {}",
                target_event_id
            ),
            Err(e) => warn!(
                "Failed to delete redacted comment {}: {:?}",
                target_event_id, e
            ),
        }
    }

    /// Process a space child state event (room added/removed from a Space).
    #[instrument(skip(self))]
    pub async fn process_space_child(&self, event: ParsedSpaceChild) {
        let site_id = match event.site_id {
            Some(ref id) => id.clone(),
            None => return, // Not a managed Space
        };

        // AUTO-DISCOVERY: Ensure the site itself exists in the store
        let _ = self
            .site_store
            .ensure_site_exists(&site_id, &event.space_room_id)
            .await;

        if event.is_attached {
            // Register the child room if we know its identity
            if let Some(ref child_identity) = event.child_room_identity {
                let _ = self
                    .registry_store
                    .register_room(
                        &event.child_room_id,
                        &site_id.clone().into(),
                        &child_identity.post_slug.clone().into(),
                    )
                    .await;
                info!(
                    "Registered active room {} for site {}",
                    event.child_room_id, site_id
                );
            }
        } else {
            // Mark as inactive
            let _ = self
                .registry_store
                .invalidate_room_registry(&event.child_room_id)
                .await;
            info!("Unregistered room {} from registry", event.child_room_id);
        }
    }
}

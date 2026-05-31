use cumments_core::{
    events::ProjectorEvent,
    models::Comment,
    ports::{CommentStore, IntentStore, RegistryStore, SiteStore},
};
use matrix_sdk::{
    RoomState,
    room::Room,
    ruma::{
        events::{
            room::{
                member::StrippedRoomMemberEvent,
                message::{MessageType, Relation, SyncRoomMessageEvent},
                redaction::SyncRoomRedactionEvent,
            },
            space::child::SyncSpaceChildEvent,
        },
        serde::Raw,
    },
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, instrument, warn};

/// A custom struct to extract our fingerprint from the Matrix event content.
#[derive(Deserialize)]
struct CustomMessageContent {
    #[serde(rename = "cumments_author_fingerprint")]
    author_fingerprint: Option<String>,
}

/// A custom struct to extract our metadata from room state.
#[derive(Deserialize)]
struct RoomMetadata {
    site_id: String,
    post_slug: Option<String>,
}

/// The Projector is an engine that observes Matrix events and
/// projects them into the local Read Store.
pub struct Projector {
    client: matrix_sdk::Client,
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
    comment_store: Arc<dyn CommentStore>,
    intent_store: Arc<dyn IntentStore>,
    event_bus: broadcast::Sender<ProjectorEvent>,
}

impl Projector {
    /// Creates a new Projector using an existing Matrix client and an event bus.
    pub fn new(
        client: matrix_sdk::Client,
        site_store: Arc<dyn SiteStore>,
        registry_store: Arc<dyn RegistryStore>,
        comment_store: Arc<dyn CommentStore>,
        intent_store: Arc<dyn IntentStore>,
        event_bus: broadcast::Sender<ProjectorEvent>,
    ) -> Self {
        Self {
            client,
            site_store,
            registry_store,
            comment_store,
            intent_store,
            event_bus,
        }
    }

    /// Registers the event handlers on the Matrix client.
    pub fn register_handlers(&self) {
        info!("Registering projectionist event handlers...");

        let site_store = self.site_store.clone();
        let registry_store = self.registry_store.clone();
        let comment_store = self.comment_store.clone();
        let intent_store = self.intent_store.clone();
        let bus = self.event_bus.clone();

        {
            let site_store = site_store.clone();
            let registry_store = registry_store.clone();
            let comment_store = comment_store.clone();
            let intent_store = intent_store.clone();
            let bus = bus.clone();
            self.client
                .add_event_handler(move |event: Raw<SyncRoomMessageEvent>, room: Room| {
                    let site_store = site_store.clone();
                    let registry_store = registry_store.clone();
                    let comment_store = comment_store.clone();
                    let intent_store = intent_store.clone();
                    let bus = bus.clone();
                    async move {
                        if let Ok(event) = event.deserialize() {
                            on_room_message(
                                event,
                                room,
                                site_store,
                                registry_store,
                                comment_store,
                                intent_store,
                                bus,
                            )
                            .await;
                        }
                    }
                });
        }

        {
            let intent_store = intent_store.clone();
            let comment_store = comment_store.clone();
            let bus = bus.clone();
            self.client
                .add_event_handler(move |event: SyncRoomRedactionEvent, room: Room| {
                    let intent_store = intent_store.clone();
                    let comment_store = comment_store.clone();
                    let bus = bus.clone();
                    async move {
                        on_room_redaction(event, room, comment_store, intent_store, bus).await;
                    }
                });
        }

        // Registry Handler: Watch for Space children changes
        {
            let site_store = site_store.clone();
            let registry_store = registry_store.clone();
            self.client.add_event_handler(
                move |event: Raw<matrix_sdk::ruma::events::AnySyncStateEvent>, room: Room| {
                    let site_store = site_store.clone();
                    let registry_store = registry_store.clone();
                    async move {
                        if let Ok(ev) = event.deserialize() {
                            use matrix_sdk::ruma::events::AnySyncStateEvent;
                            if let AnySyncStateEvent::SpaceChild(child_ev) = ev {
                                on_space_child(child_ev, room, site_store, registry_store).await;
                            }
                        }
                    }
                },
            );
        }

        self.client
            .add_event_handler(|_event: StrippedRoomMemberEvent, room: Room| async move {
                on_invited_room(room).await;
            });
    }
}

/// The event handler for Space children changes (Registry).
#[instrument(skip_all)]
#[allow(clippy::collapsible_if)]
async fn on_space_child(
    event: SyncSpaceChildEvent,
    room: Room,
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
) {
    // 1. Identify which site this Space belongs to
    let site_id = if let Ok(Some(meta_ev)) = room
        .get_state_event("im.cumments.metadata".into(), "")
        .await
    {
        serde_json::to_value(&meta_ev)
            .ok()
            .and_then(|v| v.get("content").cloned())
            .and_then(|c| serde_json::from_value::<RoomMetadata>(c).ok())
            .map(|m| m.site_id)
    } else {
        None
    };

    let site_id = match site_id {
        Some(id) => id,
        None => return, // Not a managed Space
    };

    // AUTO-DISCOVERY: Ensure the site itself exists in the store
    // This handles the case where the space exists on Matrix but not in local DB
    let room_id_owned = room.room_id().to_string();
    let _ = site_store
        .ensure_site_exists(&site_id, &room_id_owned)
        .await;

    let child_room_id_str = event.state_key().to_string();

    // Determine if it was added or removed
    if let SyncSpaceChildEvent::Original(msg) = event {
        if !msg.content.via.is_empty() {
            // Register/Update the room in registry
            if let Ok(child_room_id) =
                matrix_sdk::ruma::OwnedRoomId::try_from(child_room_id_str.clone())
            {
                if let Some(child_room) = room.client().get_room(&child_room_id) {
                    if let Some((_, post_slug)) = get_room_identity(&child_room).await {
                        let _ = registry_store
                            .register_room(
                                &child_room_id_str,
                                &site_id.clone().into(),
                                &post_slug.into(),
                            )
                            .await;
                        info!(
                            "Registered active room {} for site {}",
                            child_room_id_str, site_id
                        );
                    }
                }
            }
        } else {
            // Mark as inactive
            let _ = registry_store
                .invalidate_room_registry(&child_room_id_str)
                .await;
            info!("Unregistered room {} from registry", child_room_id_str);
        }
    }
}

async fn on_invited_room(room: Room) {
    if room.state() == RoomState::Invited {
        info!("Got invited to room {}", room.room_id());
        if let Err(e) = room.join().await {
            warn!("Failed to join invited room {}: {}", room.room_id(), e);
        } else {
            info!("Successfully joined invited room {}", room.room_id());
        }
    }
}

/// Extracts site_id and post_slug from room state metadata or falls back to alias parsing.
async fn get_room_identity(room: &Room) -> Option<(String, String)> {
    // 1. Try metadata state event first (im.cumments.metadata)
    if let Ok(Some(ev)) = room
        .get_state_event("im.cumments.metadata".into(), "")
        .await
    {
        // Since the Enum is private, we'll use the JSON representation
        // to bypass the type system safely.
        if let Ok(v) = serde_json::to_value(&ev) {
            if let Some(content) = v.get("content") {
                if let Ok(m) = serde_json::from_value::<RoomMetadata>(content.clone()) {
                    if let Some(slug) = m.post_slug {
                        return Some((m.site_id, slug));
                    }
                }
            }
        }
    }

    // 2. Fallback to alias parsing for legacy compatibility
    let alias = room.canonical_alias()?;
    let alias_str = alias.as_str();

    // Parse site_id and post_slug from alias.
    // Supports #cumments_SITE_ID_POST_SLUG:domain and #SITE_ID_POST_SLUG:domain
    let localpart = alias_str.split(':').next()?.strip_prefix('#')?;

    let content_part = localpart.strip_prefix("cumments_").unwrap_or(localpart);
    let parts: Vec<_> = content_part.splitn(2, '_').collect();

    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

/// The event handler for room message events (including edits).
#[instrument(skip_all, fields(event_id = ?event.event_id()))]
async fn on_room_message(
    event: SyncRoomMessageEvent,
    room: Room,
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
    comment_store: Arc<dyn CommentStore>,
    intent_store: Arc<dyn IntentStore>,
    bus: broadcast::Sender<ProjectorEvent>,
) {
    if let SyncRoomMessageEvent::Original(msg) = event {
        // --- PRINCIPLE B: REGISTRY ENFORCEMENT ---
        let room_id_str = room.room_id().to_string();
        let registry_status = registry_store
            .is_room_active(&room_id_str)
            .await
            .unwrap_or(None);

        match registry_status {
            Some(true) => {
                // Room is active, proceed normally
            }
            Some(false) => {
                // Room exists but is explicitly INACTIVE (tombstoned).
                // We MUST respect this to prevent zombie rooms from resurrecting.
                debug!("Ignoring message from deactivated room {}", room_id_str);
                return;
            }
            None => {
                // Room is completely UNKNOWN to us.
                // Attempt just-in-time registration if it has valid metadata.
                if let Some((site_id, post_slug)) = get_room_identity(&room).await {
                    // AUTO-DISCOVERY: Ensure the site itself exists in the store
                    let room_id_owned = room.room_id().to_string();
                    let _ = site_store
                        .ensure_site_exists(&site_id, &room_id_owned)
                        .await;

                    let _ = registry_store
                        .register_room(
                            &room_id_str,
                            &site_id.clone().into(),
                            &post_slug.clone().into(),
                        )
                        .await;
                    info!(
                        "Just-in-time registered new room {} for site {}",
                        room_id_str, site_id
                    );
                } else {
                    debug!("Ignoring message from unregistered room {}", room_id_str);
                    return;
                }
            }
        }

        // 0. Identify the room context
        let (site_id, post_slug) = match get_room_identity(&room).await {
            Some(id) => id,
            None => return, // Not a cumments room
        };

        // 1. Closed-loop: Mark any waiting intent as completed
        let event_id = msg.event_id;
        let event_id_str = event_id.to_string();
        if let Err(e) = intent_store.mark_post_intent_completed(&event_id_str).await {
            debug!(
                "Failed to mark intent as completed (normal if external msg): {:?}",
                e
            );
        }

        // Handle Edits (Replacements)
        if let Some(Relation::Replacement(replacement)) = &msg.content.relates_to {
            let target_event_id = &replacement.event_id;
            if let MessageType::Text(text) = &replacement.new_content.msgtype {
                info!("Handling edit for event {}", target_event_id);
                let content = text.body.clone();
                let target_event_id_str = target_event_id.to_string();

                match comment_store
                    .update_comment_content(&target_event_id_str, &content)
                    .await
                {
                    Ok(true) => {
                        info!("Successfully updated comment {}", target_event_id);

                        // Try to fetch updated comment to emit full object
                        if let Ok(Some(comment)) =
                            comment_store.get_comment(&target_event_id_str).await
                        {
                            let _ = bus.send(ProjectorEvent::CommentUpdated {
                                site_id,
                                post_slug,
                                comment,
                            });
                        }
                    }
                    Ok(false) => debug!("Edit received for unknown comment {}", target_event_id),
                    Err(e) => warn!("Failed to update comment {}: {:?}", target_event_id, e),
                }
            }
            return;
        }

        // Handle Original Posts
        if let MessageType::Text(text) = &msg.content.msgtype {
            let author_mxid = msg.sender;
            let author = if let Ok(Some(member)) = room.get_member(&author_mxid).await {
                member.display_name().map(ToString::to_string)
            } else {
                None
            };

            // Extract fingerprint from custom field
            let fingerprint = serde_json::to_value(&msg.content)
                .ok()
                .and_then(|v| serde_json::from_value::<CustomMessageContent>(v).ok())
                .and_then(|c| c.author_fingerprint);

            let content = text.body.clone();
            let timestamp_dt =
                chrono::DateTime::from_timestamp_millis(msg.origin_server_ts.0.into()).unwrap();

            let room_id_str = room.room_id().to_string();

            let comment = Comment {
                event_id: event_id_str.clone(),
                site_id: site_id.clone(),
                post_slug: post_slug.clone(),
                author_nickname: author.clone(),
                author_fingerprint: fingerprint.clone(),
                content: content.clone(),
                timestamp: timestamp_dt,
            };

            match comment_store
                .save_comment(
                    &comment,
                    &room_id_str,
                    &site_id.clone().into(),
                    &post_slug.clone().into(),
                )
                .await
            {
                Ok(_) => {
                    info!("Successfully projected comment event {}", event_id);
                    let _ = bus.send(ProjectorEvent::NewComment {
                        site_id,
                        post_slug,
                        comment,
                    });
                }
                Err(e) => debug!("Failed to project comment event {}: {:?}", event_id, e),
            }
        }
    }
}

/// The event handler for redactions (deletions).
#[instrument(skip_all, fields(event_id = ?event.event_id()))]
async fn on_room_redaction(
    event: SyncRoomRedactionEvent,
    room: Room,
    comment_store: Arc<dyn CommentStore>,
    intent_store: Arc<dyn IntentStore>,
    bus: broadcast::Sender<ProjectorEvent>,
) {
    if let SyncRoomRedactionEvent::Original(msg) = event {
        // redaction event can have target event id in content or in the top level redacts field
        let target_event_id = msg.redacts.as_ref().or(msg.content.redacts.as_ref());

        if let Some(target_event_id) = target_event_id {
            let target_event_id_str = target_event_id.to_string();

            // 0. Closed-loop: Mark delete intent as completed
            if let Err(e) = intent_store
                .mark_delete_intent_completed(&target_event_id_str)
                .await
            {
                debug!("Failed to mark delete intent as completed: {:?}", e);
            }

            info!(
                "Handling redaction for event {} in room {}",
                target_event_id,
                room.room_id()
            );

            // Fetch site_id and post_slug before deleting to emit event
            let comment = comment_store
                .get_comment(&target_event_id_str)
                .await
                .ok()
                .flatten();

            match comment_store.delete_comment(&target_event_id_str).await {
                Ok(true) => {
                    info!("Successfully deleted redacted comment {}", target_event_id);
                    if let Some(c) = comment {
                        let _ = bus.send(ProjectorEvent::CommentDeleted {
                            site_id: c.site_id,
                            post_slug: c.post_slug,
                            event_id: target_event_id_str,
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
    }
}

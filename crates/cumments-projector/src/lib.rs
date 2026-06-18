//! SyncAdapter – matrix-sdk based event receiver for bot mode.
//!
//! This module bridges matrix-sdk's sync event system with the
//! transport-agnostic [`EventProcessor`]. It is one of two event
//! receivers; the other is the AppService PushReceiver.

pub mod event_processor;
pub mod push_receiver;

use event_processor::{
    EventProcessor, ParsedRelation, ParsedRoomMessage, ParsedRoomRedaction, ParsedSpaceChild,
    RoomIdentity, parse_room_identity, parse_site_id_from_metadata,
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
use tracing::{info, instrument, warn};

/// A custom struct to extract our fingerprint from the Matrix event content.
#[derive(Deserialize)]
struct CustomMessageContent {
    #[serde(rename = "cumments_author_fingerprint")]
    author_fingerprint: Option<String>,
}

/// The sync-based Projector – observes Matrix events via matrix-sdk's sync
/// and feeds them into the [`EventProcessor`].
pub struct Projector {
    client: matrix_sdk::Client,
    processor: Arc<EventProcessor>,
}

impl Projector {
    /// Creates a new Projector using an existing Matrix client and pre-built EventProcessor.
    pub fn new(client: matrix_sdk::Client, processor: Arc<EventProcessor>) -> Self {
        Self { client, processor }
    }

    /// Registers the event handlers on the Matrix client.
    pub fn register_handlers(&self) {
        info!("Registering sync event handlers (SyncAdapter)...");

        let processor = self.processor.clone();

        // ── Room message handler (new comments and edits) ──
        {
            let processor = processor.clone();
            self.client
                .add_event_handler(move |event: Raw<SyncRoomMessageEvent>, room: Room| {
                    let processor = processor.clone();
                    async move {
                        if let Ok(event) = event.deserialize() {
                            on_sync_room_message(event, room, processor).await;
                        }
                    }
                });
        }

        // ── Redaction handler (comment deletions) ──
        {
            let processor = processor.clone();
            self.client
                .add_event_handler(move |event: SyncRoomRedactionEvent, room: Room| {
                    let processor = processor.clone();
                    async move {
                        on_sync_room_redaction(event, room, processor).await;
                    }
                });
        }

        // ── Space child handler (room registry updates) ──
        {
            let processor = processor.clone();
            self.client.add_event_handler(
                move |event: Raw<matrix_sdk::ruma::events::AnySyncStateEvent>, room: Room| {
                    let processor = processor.clone();
                    async move {
                        if let Ok(ev) = event.deserialize() {
                            use matrix_sdk::ruma::events::AnySyncStateEvent;
                            if let AnySyncStateEvent::SpaceChild(child_ev) = ev {
                                on_sync_space_child(child_ev, room, processor).await;
                            }
                        }
                    }
                },
            );
        }

        // ── Invite handler (auto-join) – purely sync-specific ──
        self.client
            .add_event_handler(|_event: StrippedRoomMemberEvent, room: Room| async move {
                on_invited_room(room).await;
            });
    }
}

// ── Sync adapter helpers ──────────────────────────────────────────

/// Resolve room metadata JSON from a matrix-sdk Room by querying state events.
async fn get_room_metadata_json(room: &Room) -> Option<String> {
    if let Ok(Some(ev)) = room
        .get_state_event("im.cumments.metadata".into(), "")
        .await
    {
        if let Ok(v) = serde_json::to_value(&ev) {
            if let Some(content) = v.get("content") {
                return serde_json::to_string(content).ok();
            }
        }
    }
    None
}

/// Resolve a room identity from a matrix-sdk Room (sync path).
async fn resolve_sync_room_identity(room: &Room) -> Option<RoomIdentity> {
    let metadata_json = get_room_metadata_json(room).await;
    let alias = room.canonical_alias().map(|a| a.to_string());
    parse_room_identity(metadata_json.as_deref(), alias.as_deref())
}

// ── Sync event handler implementations ────────────────────────────

/// Sync adapter: room message event → parse → delegate to EventProcessor.
#[instrument(skip_all, fields(event_id = ?event.event_id()))]
async fn on_sync_room_message(
    event: SyncRoomMessageEvent,
    room: Room,
    processor: Arc<EventProcessor>,
) {
    if let SyncRoomMessageEvent::Original(msg) = event {
        let room_id = room.room_id().to_string();
        let event_id = msg.event_id.to_string();
        let sender = msg.sender.to_string();
        let room_identity = resolve_sync_room_identity(&room).await;

        // ── Extract message body and relation ──
        let (content, relates_to) = match &msg.content.msgtype {
            MessageType::Text(text) => {
                let relates_to = msg.content.relates_to.as_ref().and_then(|rel| {
                    if let Relation::Replacement(replacement) = rel {
                        if let MessageType::Text(rt) = &replacement.new_content.msgtype {
                            Some(ParsedRelation {
                                target_event_id: replacement.event_id.to_string(),
                                new_content: rt.body.clone(),
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });
                (text.body.clone(), relates_to)
            }
            _ => return, // Not a text message, ignore
        };

        // ── Author display name ──
        // Use the original sender MXID from the event for member lookup
        let author_display_name = if let Ok(Some(member)) = room.get_member(&msg.sender).await {
            member.display_name().map(|s| s.to_string())
        } else {
            None
        };

        // ── Fingerprint from custom content field ──
        let fingerprint = serde_json::to_value(&msg.content)
            .ok()
            .and_then(|v| serde_json::from_value::<CustomMessageContent>(v).ok())
            .and_then(|c| c.author_fingerprint);

        let origin_server_ts: i64 = msg.origin_server_ts.0.into();

        let parsed = ParsedRoomMessage {
            room_id,
            event_id,
            sender,
            content,
            author_display_name,
            fingerprint,
            origin_server_ts,
            relates_to,
            room_identity,
        };

        processor.process_room_message(parsed).await;
    }
}

/// Sync adapter: redaction event → parse → delegate to EventProcessor.
#[instrument(skip_all, fields(event_id = ?event.event_id()))]
async fn on_sync_room_redaction(
    event: SyncRoomRedactionEvent,
    room: Room,
    processor: Arc<EventProcessor>,
) {
    if let SyncRoomRedactionEvent::Original(msg) = event {
        let redacts = msg
            .redacts
            .as_ref()
            .or(msg.content.redacts.as_ref())
            .map(|e| e.to_string());

        let parsed = ParsedRoomRedaction {
            room_id: room.room_id().to_string(),
            event_id: msg.event_id.to_string(),
            redacts,
            room_identity: resolve_sync_room_identity(&room).await,
        };

        processor.process_room_redaction(parsed).await;
    }
}

/// Sync adapter: space child event → parse → delegate to EventProcessor.
#[instrument(skip_all)]
async fn on_sync_space_child(
    event: SyncSpaceChildEvent,
    room: Room,
    processor: Arc<EventProcessor>,
) {
    // Resolve site_id from the Space's own metadata
    let site_id = get_room_metadata_json(&room)
        .await
        .and_then(|json| parse_site_id_from_metadata(&json));

    let child_room_id = event.state_key().to_string();
    let is_attached = match &event {
        SyncSpaceChildEvent::Original(msg) => !msg.content.via.is_empty(),
        _ => false,
    };

    // Resolve child room identity if the room is known to the client
    let child_room_identity =
        if let Ok(room_id) = matrix_sdk::ruma::OwnedRoomId::try_from(child_room_id.clone()) {
            if let Some(child_room) = room.client().get_room(&room_id) {
                resolve_sync_room_identity(&child_room).await
            } else {
                None
            }
        } else {
            None
        };

    let parsed = ParsedSpaceChild {
        space_room_id: room.room_id().to_string(),
        site_id,
        child_room_id,
        is_attached,
        child_room_identity,
    };

    processor.process_space_child(parsed).await;
}

/// Auto-join rooms when invited (sync-only concern).
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

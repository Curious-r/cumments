use cumments_core::{events::ProjectorEvent, models::Comment, ports::IntentStore};
use matrix_sdk::{
    RoomState,
    room::Room,
    ruma::{
        events::room::{
            member::StrippedRoomMemberEvent,
            message::{MessageType, Relation, SyncRoomMessageEvent},
            redaction::SyncRoomRedactionEvent,
        },
        serde::Raw,
    },
};
use serde::Deserialize;
use sqlx::SqlitePool;
use sqlx::types::chrono::NaiveDateTime;
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
// ... (rest of imports and struct)

pub struct Projector {
    client: matrix_sdk::Client,
    pool: SqlitePool,
    intent_store: Arc<dyn IntentStore>,
    event_bus: broadcast::Sender<ProjectorEvent>,
}

impl Projector {
    /// Creates a new Projector using an existing Matrix client and an event bus.
    pub fn new(
        client: matrix_sdk::Client,
        pool: SqlitePool,
        intent_store: Arc<dyn IntentStore>,
        event_bus: broadcast::Sender<ProjectorEvent>,
    ) -> Self {
        Self {
            client,
            pool,
            intent_store,
            event_bus,
        }
    }

    /// Registers the event handlers on the Matrix client.
    pub fn register_handlers(&self) {
        info!("Registering projectionist event handlers...");

        let pool = self.pool.clone();
        let bus = self.event_bus.clone();
        let intents = self.intent_store.clone();
        self.client
            .add_event_handler(move |event: Raw<SyncRoomMessageEvent>, room: Room| {
                let pool = pool.clone();
                let bus = bus.clone();
                let intents = intents.clone();
                async move {
                    if let Ok(event) = event.deserialize() {
                        on_room_message(event, room, pool, bus, intents).await;
                    }
                }
            });
        // ... (rest of the file)

        let pool = self.pool.clone();
        let bus = self.event_bus.clone();
        let intents = self.intent_store.clone();
        self.client
            .add_event_handler(move |event: SyncRoomRedactionEvent, room: Room| {
                let pool = pool.clone();
                let bus = bus.clone();
                let intents = intents.clone();
                async move {
                    on_room_redaction(event, room, pool, bus, intents).await;
                }
            });

        self.client
            .add_event_handler(|_event: StrippedRoomMemberEvent, room: Room| async move {
                on_invited_room(room).await;
            });
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
        if let Ok(json_str) = serde_json::to_string(&ev) {
            if let Ok(m) = serde_json::from_str::<RoomMetadata>(&json_str) {
                if let Some(slug) = m.post_slug {
                    return Some((m.site_id, slug));
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
    pool: SqlitePool,
    bus: broadcast::Sender<ProjectorEvent>,
    intents: Arc<dyn IntentStore>,
) {
    if let SyncRoomMessageEvent::Original(msg) = event {
        // 0. Identify the room context
        let (site_id, post_slug) = match get_room_identity(&room).await {
            Some(id) => id,
            None => return, // Not a cumments room
        };

        // 1. Closed-loop: Mark any waiting intent as completed
        let event_id = msg.event_id;
        let event_id_str = event_id.to_string();
        if let Err(e) = intents.mark_post_intent_completed(&event_id_str).await {
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

                match sqlx::query!(
                    "UPDATE comments SET content = ? WHERE event_id = ?",
                    content,
                    target_event_id_str
                )
                .execute(&pool)
                .await
                {
                    Ok(result) if result.rows_affected() > 0 => {
                        info!("Successfully updated comment {}", target_event_id);

                        // Try to fetch updated comment to emit full object
                        if let Ok(row) = sqlx::query_as!(
                            crate::CommentRow,
                            r#"SELECT event_id, author_nickname, author_fingerprint, content, timestamp as "timestamp: NaiveDateTime", site_id, post_slug FROM comments WHERE event_id = ?"#,
                            target_event_id_str
                        )
                        .fetch_one(&pool)
                        .await
                        {
                            let _ = bus.send(ProjectorEvent::CommentUpdated {
                                site_id: row.site_id,
                                post_slug: row.post_slug,
                                comment: Comment {
                                    event_id: row.event_id,
                                    author_nickname: row.author_nickname,
                                    author_fingerprint: row.author_fingerprint,
                                    content: row.content,
                                    timestamp: chrono::DateTime::from_naive_utc_and_offset(row.timestamp, chrono::Utc),
                                },
                            });
                        }
                    }
                    Ok(_) => debug!("Edit received for unknown comment {}", target_event_id),
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
                chrono::DateTime::from_timestamp_millis(msg.origin_server_ts.0.into())
                    .unwrap()
                    .naive_utc();

            let room_id_str = room.room_id().to_string();
            let author_mxid_str = author_mxid.to_string();

            match sqlx::query!(
                r#"
                INSERT INTO comments (event_id, room_id, site_id, post_slug, author_mxid, author_nickname, author_fingerprint, content, timestamp)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                event_id_str,
                room_id_str,
                site_id,
                post_slug,
                author_mxid_str,
                author,
                fingerprint,
                content,
                timestamp_dt,
            )
            .execute(&pool)
            .await
            {
                Ok(_) => {
                    info!("Successfully projected comment event {}", event_id);
                    let _ = bus.send(ProjectorEvent::NewComment {
                        site_id,
                        post_slug,
                        comment: Comment {
                            event_id: event_id_str,
                            author_nickname: author,
                            author_fingerprint: fingerprint,
                            content,
                            timestamp: chrono::DateTime::from_naive_utc_and_offset(timestamp_dt, chrono::Utc),
                        },
                    });
                },
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
    pool: SqlitePool,
    bus: broadcast::Sender<ProjectorEvent>,
    intents: Arc<dyn IntentStore>,
) {
    if let SyncRoomRedactionEvent::Original(msg) = event {
        // redaction event can have target event id in content or in the top level redacts field
        let target_event_id = msg.redacts.as_ref().or(msg.content.redacts.as_ref());

        if let Some(target_event_id) = target_event_id {
            let target_event_id_str = target_event_id.to_string();

            // 0. Closed-loop: Mark delete intent as completed
            if let Err(e) = intents
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
            let meta = sqlx::query!(
                "SELECT site_id, post_slug FROM comments WHERE event_id = ?",
                target_event_id_str
            )
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

            match sqlx::query!(
                "DELETE FROM comments WHERE event_id = ?",
                target_event_id_str
            )
            .execute(&pool)
            .await
            {
                Ok(result) if result.rows_affected() > 0 => {
                    info!("Successfully deleted redacted comment {}", target_event_id);
                    if let Some(meta) = meta {
                        let _ = bus.send(ProjectorEvent::CommentDeleted {
                            site_id: meta.site_id,
                            post_slug: meta.post_slug,
                            event_id: target_event_id_str,
                        });
                    }
                }
                Ok(_) => debug!(
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

// Internal helper for fetching updated comments
#[derive(sqlx::FromRow)]
struct CommentRow {
    event_id: String,
    author_nickname: Option<String>,
    author_fingerprint: Option<String>,
    content: String,
    timestamp: NaiveDateTime,
    site_id: String,
    post_slug: String,
}

use cumments_core::{events::ProjectorEvent, models::Comment};
use matrix_sdk::{
    RoomState,
    room::Room,
    ruma::events::room::{
        member::StrippedRoomMemberEvent,
        message::{MessageType, Relation, SyncRoomMessageEvent},
        redaction::SyncRoomRedactionEvent,
    },
};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tracing::{debug, info, instrument, warn};

/// The Projectionist is responsible for listening to Matrix events and
/// updating the read-only database tables.
#[derive(Clone)]
pub struct Projector {
    client: matrix_sdk::Client,
    pool: SqlitePool,
    event_bus: broadcast::Sender<ProjectorEvent>,
}

impl Projector {
    /// Creates a new Projector using an existing Matrix client and an event bus.
    pub fn new(
        client: matrix_sdk::Client,
        pool: SqlitePool,
        event_bus: broadcast::Sender<ProjectorEvent>,
    ) -> Self {
        Self {
            client,
            pool,
            event_bus,
        }
    }

    /// Registers the event handlers on the Matrix client.
    pub fn register_handlers(&self) {
        info!("Registering projectionist event handlers...");

        let pool = self.pool.clone();
        let bus = self.event_bus.clone();
        self.client
            .add_event_handler(move |event: SyncRoomMessageEvent, room: Room| {
                let pool = pool.clone();
                let bus = bus.clone();
                async move {
                    on_room_message(event, room, pool, bus).await;
                }
            });

        let pool = self.pool.clone();
        let bus = self.event_bus.clone();
        self.client
            .add_event_handler(move |event: SyncRoomRedactionEvent, room: Room| {
                let pool = pool.clone();
                let bus = bus.clone();
                async move {
                    on_room_redaction(event, room, pool, bus).await;
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

/// The event handler for room message events (including edits).
#[instrument(skip_all, fields(event_id = ?event.event_id()))]
async fn on_room_message(
    event: SyncRoomMessageEvent,
    room: Room,
    pool: SqlitePool,
    bus: broadcast::Sender<ProjectorEvent>,
) {
    if let SyncRoomMessageEvent::Original(msg) = event {
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
                            "SELECT event_id, author_nickname, content, timestamp, site_id, post_slug FROM comments WHERE event_id = ?",
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
            let event_id = msg.event_id;
            let alias = if let Some(alias) = room.canonical_alias() {
                alias
            } else {
                warn!(
                    "Room '{}' has no canonical alias. Skipping.",
                    room.room_id()
                );
                return;
            };

            // Parse site_id and post_slug from alias: #cumments_SITE_SLUG:domain
            let alias_str = alias.as_str();
            let parts: Vec<_> = alias_str
                .strip_prefix("#cumments_")
                .and_then(|s| s.split_once(':'))
                .map(|(p, _)| p.splitn(2, '_').collect())
                .unwrap_or_default();
            if parts.len() != 2 {
                warn!(
                    "Room alias '{}' does not match expected format. Skipping.",
                    alias_str
                );
                return;
            }
            let site_id = parts[0].to_string();
            let post_slug = parts[1].to_string();

            let author_mxid = msg.sender;
            let author = if let Ok(Some(member)) = room.get_member(&author_mxid).await {
                member.display_name().map(ToString::to_string)
            } else {
                None
            };

            let content = text.body.clone();
            let timestamp_dt =
                chrono::DateTime::from_timestamp_millis(msg.origin_server_ts.0.into())
                    .unwrap()
                    .naive_utc();

            let event_id_str = event_id.to_string();
            let room_id_str = room.room_id().to_string();
            let author_mxid_str = author_mxid.to_string();

            match sqlx::query!(
                r#"
                INSERT INTO comments (event_id, room_id, site_id, post_slug, author_mxid, author_nickname, content, timestamp)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                event_id_str,
                room_id_str,
                site_id,
                post_slug,
                author_mxid_str,
                author,
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
) {
    if let SyncRoomRedactionEvent::Original(msg) = event {
        // redaction event can have target event id in content or in the top level redacts field
        let target_event_id = msg.redacts.as_ref().or(msg.content.redacts.as_ref());

        if let Some(target_event_id) = target_event_id {
            info!(
                "Handling redaction for event {} in room {}",
                target_event_id,
                room.room_id()
            );

            let target_event_id_str = target_event_id.to_string();

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
    content: String,
    timestamp: chrono::NaiveDateTime,
    site_id: String,
    post_slug: String,
}

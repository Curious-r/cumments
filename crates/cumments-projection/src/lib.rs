use matrix_sdk::{
    Client, RoomState,
    room::Room,
    ruma::events::room::{
        member::StrippedRoomMemberEvent,
        message::{MessageType, SyncRoomMessageEvent},
    },
};
use sqlx::SqlitePool;
use tracing::{debug, info, instrument, warn};

/// The Projectionist is responsible for listening to Matrix events and
/// updating the read-only database tables.
#[derive(Clone)]
pub struct Projection {
    client: Client,
    pool: SqlitePool,
}

impl Projection {
    /// Creates a new Projection using an existing Matrix client.
    pub fn new(client: Client, pool: SqlitePool) -> Self {
        Self { client, pool }
    }

    /// Registers the event handlers on the Matrix client.
    pub fn register_handlers(&self) {
        info!("Registering projectionist event handlers...");

        let pool = self.pool.clone();
        self.client
            .add_event_handler(move |event: SyncRoomMessageEvent, room: Room| {
                let pool = pool.clone();
                async move {
                    on_room_message(event, room, pool).await;
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

/// The event handler for room message events.
#[instrument(skip_all, fields(event_id = ?event.event_id()))]
async fn on_room_message(event: SyncRoomMessageEvent, room: Room, pool: SqlitePool) {
    if let SyncRoomMessageEvent::Original(msg) = event {
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
            let site_id = parts[0];
            let post_slug = parts[1];

            let author_mxid = msg.sender;
            let author = if let Ok(Some(member)) = room.get_member(&author_mxid).await {
                member.display_name().map(ToString::to_string)
            } else {
                None
            };

            let content = text.body.clone();
            let timestamp = chrono::DateTime::from_timestamp_millis(msg.origin_server_ts.0.into())
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
                timestamp,
            )
            .execute(&pool)
            .await
            {
                Ok(_) => info!("Successfully projected comment event {}", event_id),
                // This can fail if the event is a duplicate, which is fine.
                Err(e) => debug!("Failed to project comment event {}: {:?}", event_id, e),
            }
        }
    }
}

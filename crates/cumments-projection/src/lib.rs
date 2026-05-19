use anyhow::Result;
use matrix_sdk::{
    Client, RoomState, SessionMeta,
    authentication::SessionTokens,
    authentication::matrix::MatrixSession as Session,
    config::SyncSettings,
    event_handler::Ctx,
    room::Room,
    ruma::events::{
        AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent,
        room::member::StrippedRoomMemberEvent, room::message::MessageType,
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

impl Projection {
    /// Creates a new Projection.
    pub async fn new(
        pool: SqlitePool,
        homeserver_url: &str,
        user: &str,
        token: &str,
        device_id: Option<&str>,
    ) -> Result<Self> {
        let client = Client::builder()
            .homeserver_url(homeserver_url)
            .build()
            .await?;

        // Restore the login session
        let device_id =
            device_id.expect("device_id is required for session restoration in matrix-sdk 0.14.0");

        let session = Session {
            meta: SessionMeta {
                user_id: user.try_into()?,
                device_id: device_id.try_into()?,
            },
            tokens: SessionTokens {
                access_token: token.to_string(),
                refresh_token: None,
            },
        };

        client.restore_session(session).await?;
        debug!("Restored login session with device ID '{}'", device_id);

        info!(
            "Successfully restored Matrix session for user {}",
            client.user_id().unwrap()
        );

        Ok(Self { client, pool })
    }

    /// Runs the main projection loop.
    /// This will start the Matrix client's sync process and listen for events.
    #[instrument(skip(self))]
    pub async fn run(&self) -> Result<()> {
        info!("Registering event handler and starting projectionist sync loop...");

        self.client.add_event_handler_context(self.pool.clone());

        self.client.add_event_handler(
            |event: AnySyncTimelineEvent, room: Room, pool: Ctx<SqlitePool>| async move {
                on_timeline_event(event, room, pool.0).await;
            },
        );
        self.client
            .add_event_handler(|_event: StrippedRoomMemberEvent, room: Room| async move {
                on_invited_room(room).await;
            });

        let sync_settings = SyncSettings::default();
        self.client.sync(sync_settings).await?; // This will run forever

        Ok(())
    }
}

/// The event handler for all timeline events.
#[instrument(skip_all, fields(event_id = ?event.event_id()))]
async fn on_timeline_event(event: AnySyncTimelineEvent, room: Room, pool: SqlitePool) {
    let event_id = event.event_id().to_owned();

    if let AnySyncTimelineEvent::MessageLike(ev) = event {
        if let AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(msg)) = ev {
            if let MessageType::Text(text) = &msg.content.msgtype {
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
                let timestamp =
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
}

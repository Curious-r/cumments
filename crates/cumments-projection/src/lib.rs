use anyhow::Result;
use matrix_sdk::{
    config::SyncSettings,
    ruma::events::{room::message::MessageType, AnySyncTimelineEvent, SyncMessageLikeEvent},
    Client,
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
        if let Some(device_id) = device_id {
            client
                .restore_login_with_device_id(user.into(), device_id.into(), token.into())
                .await?;
            debug!("Restored login session with device ID '{}'", device_id);
        } else {
            client.restore_login(user.into(), token.into()).await?;
            debug!("Restored login session without a device ID");
        }

        info!(
            "Successfully restored Matrix session for user {}",
            client.user_id().await.unwrap()
        );

        Ok(Self { client, pool })
    }

    /// Runs the main projection loop.
    /// This will start the Matrix client's sync process and listen for events.
    #[instrument(skip(self))]
    pub async fn run(&self) -> Result<()> {
        info!("Registering event handler and starting projectionist sync loop...");

        self.client.add_event_handler_context(self.clone());
        self.client.add_event_handler(on_timeline_event);

        let sync_settings = SyncSettings::default();
        self.client.sync(sync_settings).await?; // This will run forever

        Ok(())
    }
}

/// The event handler for all timeline events.
#[instrument(skip_all, fields(event_type = event.event_type().to_string()))]
async fn on_timeline_event(event: AnySyncTimelineEvent, context: Projection) {
    let event_id = if let Some(id) = event.event_id() {
        id
    } else {
        warn!("Received an event without an event ID. Skipping.");
        return;
    };

    if let AnySyncTimelineEvent::MessageLike(SyncMessageLikeEvent::RoomMessage(msg)) = event {
        if let MessageType::Text(text) = msg.content.msgtype {
            let room = if let Some(room) = context.client.get_room(&room_id) {
                room
            } else {
                warn!(
                    "Received message for room '{}' which is not in client state. Skipping.",
                    room_id
                );
                return;
            };

            let alias = if let Some(alias) = room.canonical_alias().await {
                alias
            } else {
                warn!("Room '{}' has no canonical alias. Skipping.", room.id());
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

            let content = text.body;
            let timestamp = chrono::DateTime::from_timestamp_millis(msg.origin_server_ts.0.into())
                .unwrap()
                .naive_utc();

            match sqlx::query!(
                            r#"
                            INSERT INTO comments (event_id, room_id, site_id, post_slug, author_mxid, author_nickname, content, timestamp)
                            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                            "#,
                            event_id.to_string(),
                            room.id().to_string(),
                            site_id,
                            post_slug,
                            author_mxid.to_string(),
                            author,
                            content,
                            timestamp,
                        )
                        .execute(&context.pool)
                        .await {
                            Ok(_) => info!("Successfully projected comment event {}", event_id),
                            // This can fail if the event is a duplicate, which is fine.
                            Err(e) => debug!("Failed to project comment event {}: {:?}", event_id, e),
                        }
        }
    }
}

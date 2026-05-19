use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::intents::{DeleteCommentIntent, PostCommentIntent};
use matrix_sdk::{
    Client, SessionMeta,
    authentication::SessionTokens,
    authentication::matrix::MatrixSession as Session,
    config::SyncSettings,
    ruma::{
        EventId, OwnedRoomAliasId,
        api::client::room::create_room::v3::{self, RoomPreset as Preset},
        events::room::message::RoomMessageEventContent,
    },
};
use tracing::{debug, info, instrument};

use crate::MatrixOperator;

/// A MatrixOperator that acts as a bot.
/// It connects to a homeserver using a user account and token.
pub struct BotOperator {
    client: Client,
}

impl BotOperator {
    /// Creates a new BotOperator by restoring a Matrix session.
    pub async fn new(
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

        // Run an initial sync to get room states, etc.
        // We do this in a background task to not block the reconciler.
        let sync_client = client.clone();
        tokio::spawn(async move {
            info!("Starting initial Matrix sync in background...");
            if let Err(e) = sync_client.sync_once(SyncSettings::default()).await {
                tracing::error!("Initial Matrix sync failed: {:?}", e);
            } else {
                info!("Initial Matrix sync completed.");
            }
        });

        Ok(Self { client })
    }
}

#[async_trait]
impl MatrixOperator for BotOperator {
    /// The room is identified by an alias in the format:
    /// `#{site_id}_{post_slug}:{homeserver_domain}`
    #[instrument(skip(self, intent), fields(site_id = %intent.site_id.as_str(), post_slug = %intent.post_slug.as_str()))]
    async fn post_comment(&self, intent: &PostCommentIntent) -> Result<String> {
        // 1. Determine the room alias based on the intent
        let homeserver = self.client.homeserver();
        let alias_localpart = format!(
            "cumments_{}_{}",
            intent.site_id.as_str(),
            intent.post_slug.as_str()
        );
        let room_alias_string = format!(
            "#cumments_{}_{}:{}",
            intent.site_id.as_str(),
            intent.post_slug.as_str(),
            homeserver.host().unwrap()
        );
        let room_alias: OwnedRoomAliasId = room_alias_string.as_str().try_into()?;

        info!("Looking for room with alias {}", room_alias);

        // 2. Try to resolve the alias to a room ID and join, or create it
        let room = if let Some(room_id) = self
            .client
            .resolve_room_alias(&room_alias)
            .await
            .ok()
            .map(|r| r.room_id)
        {
            info!("Found existing room {} for alias", room_id);
            let room = self.client.join_room_by_id(&room_id).await?;
            info!("Successfully joined room {}", room.room_id());
            room
        } else {
            info!(
                "No room found for alias '{}'. Creating a new one...",
                room_alias
            );
            let room_name = format!(
                "Comments: {}/{}",
                intent.site_id.as_str(),
                intent.post_slug.as_str()
            );
            let topic = format!(
                "Comments for post '{}' on site '{}'",
                intent.post_slug.as_str(),
                intent.site_id.as_str()
            );

            let mut request = v3::Request::new();
            request.name = Some(room_name);
            request.topic = Some(topic);
            request.room_alias_name = Some(alias_localpart.try_into()?);
            request.preset = Some(Preset::PublicChat);

            let response = self.client.create_room(request).await?;
            info!(
                "Successfully created and joined room {}",
                response.room_id()
            );
            self.client.get_room(&response.room_id()).ok_or_else(|| {
                anyhow!(
                    "Just-created room {} not found in client state",
                    response.room_id()
                )
            })?
        };

        // 3. Post the message
        let content = format!("**{}**: {}", intent.nickname, intent.content);
        let message_content = RoomMessageEventContent::text_markdown(content);

        info!("Sending message to room {}", room.room_id());
        let response = room.send(message_content).await?;
        info!(
            "Message sent successfully. Event ID: {}",
            response.response.event_id
        );

        Ok(response.response.event_id.to_string())
    }

    #[instrument(skip(self, intent), fields(site_id = %intent.site_id.as_str(), post_slug = %intent.post_slug.as_str(), event_id = %intent.event_id))]
    async fn redact_comment(&self, intent: &DeleteCommentIntent) -> Result<()> {
        let homeserver = self.client.homeserver();

        let room_alias_string = format!(
            "#cumments_{}_{}:{}",
            intent.site_id.as_str(),
            intent.post_slug.as_str(),
            homeserver.host().unwrap()
        );
        let room_alias: OwnedRoomAliasId = room_alias_string.as_str().try_into()?;

        let room_id = self
            .client
            .resolve_room_alias(&room_alias)
            .await
            .ok()
            .map(|r| r.room_id)
            .ok_or_else(|| {
                anyhow!(
                    "Cannot redact comment in room '{}' that does not exist.",
                    room_alias
                )
            })?;

        let room = self
            .client
            .get_room(&room_id)
            .ok_or_else(|| anyhow!("Room '{}' not found in client state", room_id))?;

        let event_id: &EventId = intent.event_id.as_str().try_into()?;

        info!("Redacting event '{}' in room {}", event_id, room.room_id());
        room.redact(event_id, None, None).await?;
        info!("Redaction successful.");

        Ok(())
    }
}

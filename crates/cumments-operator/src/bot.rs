use anyhow::{anyhow, Result};
use async_trait::async_trait;
use cumments_core::intents::PostCommentIntent;
use matrix_sdk::room::create::CreateRoomBuilder;
use matrix_sdk::{
    config::SyncSettings,
    ruma::{room::RoomName, OwnedRoomAliasId},
    Client,
};
use tracing::{debug, info, instrument};

use crate::MatrixOperator;

/// A MatrixOperator that acts as a bot.
/// It connects to a homeserver using a user account and token.
pub struct BotOperator {
    client: Client,
}

impl BotOperator {
    /// Creates a new BotOperator.
    /// It will attempt to log in to the homeserver and perform an initial sync.
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
        // Note: This does not verify that the token is still valid.
        // The first request will fail if it's not.
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

        // Run an initial sync to get room states, etc.
        // We run it in a separate task so the constructor can return.
        let sync_client = client.clone();
        tokio::spawn(async move {
            info!("Starting initial Matrix sync in background...");
            sync_client.sync_once(SyncSettings::default()).await;
            info!("Initial Matrix sync completed.");
        });

        Ok(Self { client })
    }
}

#[async_trait]
impl MatrixOperator for BotOperator {
    /// Posts a comment to the appropriate Matrix room.
    ///
    /// The room is identified by an alias in the format:
    /// `#{site_id}_{post_slug}:{homeserver_domain}`
    #[instrument(skip(self, intent), fields(site_id = %intent.site_id.as_str(), post_slug = %intent.post_slug.as_str()))]
    async fn post_comment(&self, intent: &PostCommentIntent) -> Result<String> {
        // 1. Determine the room alias based on the intent
        let homeserver = self
            .client
            .homeserver()
            .ok_or_else(|| anyhow!("Client is not connected to a homeserver"))?;
        let alias_localpart = format!(
            "cumments_{}_{}",
            intent.site_id.as_str(),
            intent.post_slug.as_str()
        );
        let room_alias: OwnedRoomAliasId = OwnedRoomAliasId::from_localpart_and_server_name(
            alias_localpart.clone(),
            homeserver.to_owned(),
        )?;

        info!("Looking for room with alias {}", room_alias);

        // 2. Try to resolve the alias to a room ID and join, or create it
        let room = match self.client.resolve_room_alias(&room_alias).await? {
            Some(room_id) => {
                info!("Found existing room {} for alias", room_id);
                let room = self.client.join_room_by_id(&room_id).await?;
                info!("Successfully joined room {}", room.room_id());
                room
            }
            None => {
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

                let mut builder = CreateRoomBuilder::new();
                builder
                    .name(room_name)
                    .topic(topic)
                    .room_alias_name(alias_localpart)
                    .preset(matrix_sdk::room::create::Preset::PublicChat);

                let (room_id, _) = self.client.create_room(builder).await?;
                info!("Successfully created and joined room {}", room_id);
                self.client.get_room(&room_id).ok_or_else(|| {
                    anyhow!("Just-created room {} not found in client state", room_id)
                })?
            }
        };

        // 3. Post the message
        let content = format!("**{}**: {}", intent.nickname, intent.content);
        let message_content =
            matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_markdown(
                content,
            );

        info!("Sending message to room {}", room.id());
        let response = room.send(message_content).await?;
        info!("Message sent successfully. Event ID: {}", response.event_id);

        Ok(response.event_id.to_string())
    }
}

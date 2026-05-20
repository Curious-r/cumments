use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::{
    models::{PostSlug, SiteId},
    ports::MatrixDriver,
};
use matrix_sdk::{
    Client, RoomState,
    ruma::{
        EventId, Int, OwnedRoomAliasId, OwnedRoomId, OwnedServerName, OwnedUserId,
        api::client::room::create_room::v3::{self, RoomPreset},
        events::{
            InitialStateEvent,
            room::{message::RoomMessageEventContent, power_levels::RoomPowerLevelsEventContent},
            space::{child::SpaceChildEventContent, parent::SpaceParentEventContent},
        },
        room::RoomType,
        room_version_rules::AuthorizationRules,
        serde::Raw,
    },
};
use std::str::FromStr;
use tracing::{info, instrument};

/// A MatrixDriver that acts as a bot.
pub struct BotMatrixDriver {
    client: Client,
    owner_id: OwnedUserId,
}

impl BotMatrixDriver {
    /// Creates a new BotMatrixDriver using an existing Matrix client and owner ID.
    pub fn new(client: Client, owner_id: OwnedUserId) -> Self {
        Self { client, owner_id }
    }

    /// Internal helper to get server name.
    fn server_name(&self) -> Result<OwnedServerName> {
        let homeserver = self.client.homeserver();
        OwnedServerName::from_str(&homeserver.host().unwrap().to_string())
            .map_err(|e| anyhow!("Invalid server name: {:?}", e))
    }
}

#[async_trait]
impl MatrixDriver for BotMatrixDriver {
    #[instrument(skip(self), fields(site_id = %site_id.as_str()))]
    async fn create_site_space(&self, site_id: &SiteId) -> Result<String> {
        let site_id_str = site_id.as_str();
        let alias_localpart = format!("cumments_{}", site_id_str);

        info!("Creating new space for site {}", site_id_str);

        let mut request = v3::Request::new();
        request.name = Some(format!("Comments: {}", site_id_str));
        request.room_alias_name = Some(alias_localpart.try_into()?);

        let mut creation_content = v3::CreationContent::new();
        creation_content.room_type = Some(RoomType::Space);
        request.creation_content = Some(Raw::new(&creation_content)?);

        request.invite = vec![self.owner_id.clone()];
        let mut power_levels = RoomPowerLevelsEventContent::new(&AuthorizationRules::V1);
        power_levels
            .users
            .insert(self.owner_id.clone(), Int::from(100));
        if let Some(bot_id) = self.client.user_id() {
            power_levels.users.insert(bot_id.to_owned(), Int::from(100));
        }
        let pl_event = InitialStateEvent::with_empty_state_key(power_levels);
        request.initial_state = vec![pl_event.to_raw_any()];

        let response = self.client.create_room(request).await?;
        Ok(response.room_id().to_string())
    }

    #[instrument(skip(self), fields(site_id = %site_id.as_str(), post_slug = %post_slug.as_str()))]
    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        space_id: &str,
    ) -> Result<String> {
        let homeserver_domain = self.server_name()?;
        let alias_localpart = format!("cumments_{}_{}", site_id.as_str(), post_slug.as_str());
        let room_alias_string = format!(
            "#cumments_{}_{}:{}",
            site_id.as_str(),
            post_slug.as_str(),
            homeserver_domain
        );
        let room_alias: OwnedRoomAliasId = room_alias_string.as_str().try_into()?;
        let space_room_id: OwnedRoomId = space_id.try_into()?;

        // 1. Try to resolve the alias
        if let Some(room_id) = self
            .client
            .resolve_room_alias(&room_alias)
            .await
            .ok()
            .map(|r| r.room_id)
        {
            if let Some(room) = self.client.get_room(&room_id) {
                if room.state() != RoomState::Joined {
                    room.join().await?;
                }
            } else {
                self.client.join_room_by_id(&room_id).await?;
            }
            return Ok(room_id.to_string());
        }

        // 2. Create room if not exists
        info!("Creating new comment room for {}", post_slug.as_str());
        let mut request = v3::Request::new();
        request.name = Some(format!(
            "Comments: {}/{}",
            site_id.as_str(),
            post_slug.as_str()
        ));
        request.room_alias_name = Some(alias_localpart.try_into()?);
        request.preset = Some(RoomPreset::PublicChat);
        request.invite = vec![self.owner_id.clone()];

        let mut power_levels = RoomPowerLevelsEventContent::new(&AuthorizationRules::V1);
        if let Some(bot_id) = self.client.user_id() {
            power_levels.users.insert(bot_id.to_owned(), Int::from(100));
        }
        power_levels
            .users
            .insert(self.owner_id.clone(), Int::from(100));
        let pl_event = InitialStateEvent::with_empty_state_key(power_levels);
        request.initial_state = vec![pl_event.to_raw_any()];

        let response = self.client.create_room(request).await?;
        let room_id = response.room_id().to_owned();

        // 3. Link to Space
        let server_name = self.server_name()?;

        // Add room as child of space
        if let Some(space_room) = self.client.get_room(&space_room_id) {
            let child_content = SpaceChildEventContent::new(vec![server_name.clone()]);
            let _ = space_room
                .send_state_event_for_key(&room_id, child_content)
                .await;
        }

        // Add space as parent of room
        if let Some(child_room) = self.client.get_room(&room_id) {
            let parent_content = SpaceParentEventContent::new(vec![server_name]);
            let _ = child_room
                .send_state_event_for_key(&space_room_id, parent_content)
                .await;
        }

        Ok(room_id.to_string())
    }

    #[instrument(skip(self))]
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        nickname: &str,
        fingerprint: &str,
    ) -> Result<String> {
        let room_id_owned: OwnedRoomId = room_id.try_into()?;
        let room = self
            .client
            .get_room(&room_id_owned)
            .ok_or_else(|| anyhow!("Room {} not found", room_id))?;

        let formatted_content = format!("**{}**: {}", nickname, content);

        // We use a custom JSON structure to include the fingerprint as a top-level field
        // while still being compatible with standard Matrix clients (msgtype: m.text)
        let message_json = serde_json::json!({
            "msgtype": "m.text",
            "body": formatted_content,
            "format": "org.matrix.custom.html",
            "formatted_body": format!("<strong>{}</strong>: {}", nickname, content),
            "cumments_author_fingerprint": fingerprint,
        });

        let content: RoomMessageEventContent = serde_json::from_value(message_json)?;

        let response = room.send(content).await?;
        Ok(response.response.event_id.to_string())
    }

    #[instrument(skip(self))]
    async fn redact_message(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        event_id: &str,
    ) -> Result<()> {
        let homeserver_domain = self.server_name()?;
        let room_alias_string = format!(
            "#cumments_{}_{}:{}",
            site_id.as_str(),
            post_slug.as_str(),
            homeserver_domain
        );
        let room_alias: OwnedRoomAliasId = room_alias_string.as_str().try_into()?;

        let room_id = self
            .client
            .resolve_room_alias(&room_alias)
            .await
            .ok()
            .map(|r| r.room_id)
            .ok_or_else(|| anyhow!("Room {} not found", room_alias))?;

        let room = self
            .client
            .get_room(&room_id)
            .ok_or_else(|| anyhow!("Room {} not found in state", room_id))?;

        let event_id_owned: &EventId = event_id.try_into()?;
        room.redact(event_id_owned, None, None).await?;

        Ok(())
    }
}

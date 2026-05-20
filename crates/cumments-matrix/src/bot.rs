use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::{
    models::{PostSlug, SiteId},
    ports::MatrixDriver,
};
use matrix_sdk::{
    Client, RoomState,
    ruma::{
        Int, OwnedEventId, OwnedRoomAliasId, OwnedRoomId, OwnedServerName, OwnedUserId,
        api::client::room::create_room::v3::{self, RoomPreset},
        events::{
            EmptyStateKey, InitialStateEvent,
            macros::EventContent,
            room::{message::RoomMessageEventContent, power_levels::RoomPowerLevelsEventContent},
            space::{child::SpaceChildEventContent, parent::SpaceParentEventContent},
        },
        room::RoomType,
        room_version_rules::AuthorizationRules,
        serde::Raw,
    },
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{info, instrument};

#[derive(Clone, Debug, Serialize, Deserialize, EventContent)]
#[ruma_event(type = "im.cumments.metadata", kind = State, state_key_type = EmptyStateKey)]
#[allow(unexpected_cfgs)]
struct RoomMetadataContent {
    pub site_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_slug: Option<String>,
}

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
        let host = homeserver
            .host()
            .ok_or_else(|| anyhow!("Homeserver has no host"))?;
        OwnedServerName::from_str(&host.to_string())
            .map_err(|e| anyhow!("Invalid server name: {:?}", e))
    }

    // ... helper for sending metadata
    async fn set_room_metadata(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: Option<&PostSlug>,
    ) -> Result<()> {
        let room_id_owned: OwnedRoomId = room_id.try_into()?;
        let room = self
            .client
            .get_room(&room_id_owned)
            .ok_or_else(|| anyhow!("Room {} not found while setting metadata", room_id))?;

        let content = RoomMetadataContent {
            site_id: site_id.as_str().to_string(),
            post_slug: post_slug.map(|s| s.as_str().to_string()),
        };

        // For EventContent with EmptyStateKey, we use send_state_event
        room.send_state_event(content).await?;
        Ok(())
    }
}

#[async_trait]
impl MatrixDriver for BotMatrixDriver {
    #[instrument(skip(self), fields(site_id = %site_id.as_str()))]
    async fn create_site_space(&self, site_id: &SiteId) -> Result<String> {
        let site_id_str = site_id.as_str();
        let homeserver_domain = self.server_name()?;
        let alias_localpart = format!("cumments_{}", site_id_str);
        let room_alias_string = format!("#cumments_{}:{}", site_id_str, homeserver_domain);
        let room_alias: OwnedRoomAliasId = room_alias_string.as_str().try_into()?;

        // 1. Try to resolve the alias first (Idempotency)
        let room_id = if let Some(room_id) = self
            .client
            .resolve_room_alias(&room_alias)
            .await
            .ok()
            .map(|r| r.room_id)
        {
            info!(
                "Space for site {} already exists, resolving to {}",
                site_id_str, room_id
            );
            if let Some(room) = self.client.get_room(&room_id) {
                if room.state() != RoomState::Joined {
                    room.join().await?;
                }
            } else {
                self.client.join_room_by_id(&room_id).await?;
            }
            room_id.to_string()
        } else {
            // 2. Create new space if not found
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
            response.room_id().to_string()
        };

        // Ensure metadata is set (even for existing rooms, to backfill)
        let _ = self.set_room_metadata(&room_id, site_id, None).await;

        Ok(room_id)
    }

    #[instrument(skip(self), fields(site_id = %site_id.as_str(), post_slug = %post_slug.as_str()))]
    #[allow(clippy::collapsible_if)]
    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        space_id: &str,
        candidate_room_id: Option<&str>,
    ) -> Result<String> {
        let space_room_id: OwnedRoomId = space_id.try_into()?;
        let space = self
            .client
            .get_room(&space_room_id)
            .ok_or_else(|| anyhow!("Space room {} not found", space_id))?;

        let mut target_room_id = None;

        // --- PHASE 0: O(1) DISCOVERY (Check Candidate) ---
        if let Some(candidate) = candidate_room_id {
            if let Ok(id) = OwnedRoomId::try_from(candidate) {
                if let Some(room) = self.client.get_room(&id) {
                    if let Ok(Some(meta_ev)) =
                        room.get_state_event_static::<RoomMetadataContent>().await
                    {
                        if let Ok(json_str) = serde_json::to_string(&meta_ev) {
                            if let Ok(full_json) =
                                serde_json::from_str::<serde_json::Value>(&json_str)
                            {
                                if let Some(content_val) = full_json.get("content") {
                                    if let Ok(m) = serde_json::from_value::<RoomMetadataContent>(
                                        content_val.clone(),
                                    ) {
                                        if m.site_id == site_id.as_str()
                                            && m.post_slug.as_deref() == Some(post_slug.as_str())
                                        {
                                            info!(
                                                "Validated candidate room {} via metadata",
                                                candidate
                                            );
                                            target_room_id = Some(id);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // --- PHASE 1: DISCOVERY (Traverse Space Children) ---
        if target_room_id.is_none() {
            info!("Searching for existing room in space {}", space_id);
            let child_events = space.get_state_events("m.space.child".into()).await?;

            for event in child_events {
                // Since RawAnySyncOrStrippedState is private, we use the JSON representation
                if let Ok(json_val) = serde_json::to_value(&event) {
                    let state_key = json_val.get("state_key").and_then(|v| v.as_str());
                    if let Some(sk) = state_key {
                        if let Ok(child_room_id) = OwnedRoomId::try_from(sk) {
                            // Check if this room has the correct metadata
                            if let Some(child_room) = self.client.get_room(&child_room_id) {
                                if let Ok(Some(meta_ev)) = child_room
                                    .get_state_event_static::<RoomMetadataContent>()
                                    .await
                                {
                                    if let Ok(json_str) = serde_json::to_string(&meta_ev) {
                                        if let Ok(full_json) =
                                            serde_json::from_str::<serde_json::Value>(&json_str)
                                        {
                                            if let Some(content_val) = full_json.get("content") {
                                                if let Ok(m) =
                                                    serde_json::from_value::<RoomMetadataContent>(
                                                        content_val.clone(),
                                                    )
                                                {
                                                    if m.site_id == site_id.as_str()
                                                        && m.post_slug.as_deref()
                                                            == Some(post_slug.as_str())
                                                    {
                                                        info!(
                                                            "Found matching room {} via metadata",
                                                            child_room_id
                                                        );
                                                        target_room_id = Some(child_room_id);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // --- PHASE 2: CREATION OR RESOLUTION ---
        let room_id = if let Some(id) = target_room_id {
            // Found existing room via metadata
            if let Some(room) = self.client.get_room(&id) {
                if room.state() != RoomState::Joined {
                    room.join().await?;
                }
            } else {
                self.client.join_room_by_id(&id).await?;
            }
            id
        } else {
            // No room found in space metadata, create a new one
            info!("No matching room found in space. Creating new comment room.");
            let alias_localpart = format!("cumments_{}_{}", site_id.as_str(), post_slug.as_str());

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
            let new_room_id = response.room_id().to_owned();

            // Link to Space (Registry Update)
            let server_name = self.server_name()?;
            let child_content = SpaceChildEventContent::new(vec![server_name.clone()]);
            let _ = space
                .send_state_event_for_key(&new_room_id, child_content)
                .await;

            if let Some(child_room) = self.client.get_room(&new_room_id) {
                let parent_content = SpaceParentEventContent::new(vec![server_name]);
                let _ = child_room
                    .send_state_event_for_key(&space_room_id, parent_content)
                    .await;
            }
            new_room_id
        };

        // --- PHASE 3: METADATA ENFORCEMENT ---
        let room_id_str = room_id.to_string();

        // Ensure metadata is set (Source of Truth)
        let _ = self
            .set_room_metadata(&room_id_str, site_id, Some(post_slug))
            .await;

        Ok(room_id_str)
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
    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        nickname: &str,
        fingerprint: &str,
    ) -> Result<String> {
        let room_id_owned: OwnedRoomId = room_id.try_into()?;
        let room = self
            .client
            .get_room(&room_id_owned)
            .ok_or_else(|| anyhow!("Room {} not found", room_id))?;

        let formatted_content = format!("**{}**: {}", nickname, new_content);
        let event_id_owned: OwnedEventId = event_id.try_into()?;

        // Construct m.replace relation
        let message_json = serde_json::json!({
            "msgtype": "m.text",
            "body": format!(" * {}", formatted_content),
            "m.new_content": {
                "msgtype": "m.text",
                "body": formatted_content,
                "format": "org.matrix.custom.html",
                "formatted_body": format!("<strong>{}</strong>: {}", nickname, new_content),
                "cumments_author_fingerprint": fingerprint,
            },
            "m.relates_to": {
                "rel_type": "m.replace",
                "event_id": event_id_owned,
            }
        });

        let content: RoomMessageEventContent = serde_json::from_value(message_json)?;
        let response = room.send(content).await?;
        Ok(response.response.event_id.to_string())
    }

    #[instrument(skip(self))]
    async fn redact_message(&self, room_id: &str, event_id: &str) -> Result<()> {
        let room_id_owned: OwnedRoomId = room_id.try_into()?;
        let room = self
            .client
            .get_room(&room_id_owned)
            .ok_or_else(|| anyhow!("Room {} not found in state", room_id))?;

        let event_id_owned: OwnedEventId = event_id.try_into()?;
        room.redact(&event_id_owned, None, None).await?;

        Ok(())
    }
}

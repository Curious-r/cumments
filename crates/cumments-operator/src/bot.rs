use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::{
    intents::{DeleteCommentIntent, PostCommentIntent},
    models::Site,
    ports::SiteRepository,
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
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, instrument, warn};

use crate::MatrixOperator;

/// Cache for site-to-space-id mappings.
#[derive(Clone, Default)]
pub struct SpaceCache {
    inner: Arc<RwLock<HashMap<String, OwnedRoomId>>>,
}

impl SpaceCache {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A MatrixOperator that acts as a bot.
/// It connects to a homeserver using a user account and token.
pub struct BotOperator {
    client: Client,
    owner_id: OwnedUserId,
    site_storage: Arc<dyn SiteRepository>,
    space_cache: SpaceCache,
}

impl BotOperator {
    /// Creates a new BotOperator using an existing Matrix client, owner ID and site storage.
    pub fn new(
        client: Client,
        owner_id: OwnedUserId,
        site_storage: Arc<dyn SiteRepository>,
    ) -> Self {
        Self {
            client,
            owner_id,
            site_storage,
            space_cache: SpaceCache::new(),
        }
    }

    /// Ensures a Space exists for the given site and returns its room ID.
    async fn ensure_site_space(&self, site_id: &str) -> Result<OwnedRoomId> {
        // 1. Check cache
        {
            let cache = self.space_cache.inner.read().await;
            if let Some(id) = cache.get(site_id) {
                if let Some(room) = self.client.get_room(id) {
                    if room.state() == RoomState::Joined {
                        return Ok(id.clone());
                    }
                }
            }
        }

        // 2. Check Database
        if let Some(site) = self.site_storage.get_site(site_id).await? {
            let space_id: OwnedRoomId = site.matrix_space_id.try_into()?;
            // Update cache
            self.space_cache
                .inner
                .write()
                .await
                .insert(site_id.to_string(), space_id.clone());
            return Ok(space_id);
        }

        // 3. Resolve or Create Space
        let homeserver = self.client.homeserver();
        let alias_localpart = format!("cumments_{}", site_id);
        let room_alias_string = format!("#cumments_{}:{}", site_id, homeserver.host().unwrap());
        let room_alias: OwnedRoomAliasId = room_alias_string.as_str().try_into()?;

        let space_id = if let Some(room_id) = self
            .client
            .resolve_room_alias(&room_alias)
            .await
            .ok()
            .map(|r| r.room_id)
        {
            info!("Found existing space {} for site {}", room_id, site_id);
            if let Some(room) = self.client.get_room(&room_id) {
                if room.state() != RoomState::Joined {
                    room.join().await?;
                }
            } else {
                self.client.join_room_by_id(&room_id).await?;
            }
            room_id
        } else {
            info!("Creating new space for site {}", site_id);
            let mut request = v3::Request::new();
            request.name = Some(format!("Comments: {}", site_id));
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
            response.room_id().to_owned()
        };

        // 4. Persist to database
        let site = Site {
            id: site_id.to_string(),
            matrix_space_id: space_id.to_string(),
            display_name: Some(site_id.to_string()),
            created_at: chrono::Utc::now(),
        };
        self.site_storage.save_site(&site).await?;

        // 5. Update cache
        self.space_cache
            .inner
            .write()
            .await
            .insert(site_id.to_string(), space_id.clone());

        Ok(space_id)
    }

    /// Links a room to a space.
    async fn link_room_to_space(
        &self,
        room_id: &OwnedRoomId,
        space_id: &OwnedRoomId,
    ) -> Result<()> {
        let homeserver = self.client.homeserver();
        let server_name = OwnedServerName::from_str(&homeserver.host().unwrap().to_string())?;

        if let Some(space_room) = self.client.get_room(space_id) {
            let child_content = SpaceChildEventContent::new(vec![server_name.clone()]);
            if let Err(e) = space_room
                .send_state_event_for_key(room_id, child_content)
                .await
            {
                warn!(
                    "Failed to link room {} to space {}: {:?}",
                    room_id, space_id, e
                );
            }
        }

        if let Some(child_room) = self.client.get_room(room_id) {
            let parent_content = SpaceParentEventContent::new(vec![server_name]);
            if let Err(e) = child_room
                .send_state_event_for_key(space_id, parent_content)
                .await
            {
                warn!("Failed to set space parent for room {}: {:?}", room_id, e);
            }
        }

        Ok(())
    }
}

#[async_trait]
impl MatrixOperator for BotOperator {
    /// The room is identified by an alias in the format:
    /// `#{site_id}_{post_slug}:{homeserver_domain}`
    #[instrument(skip(self, intent), fields(site_id = %intent.site_id.as_str(), post_slug = %intent.post_slug.as_str()))]
    async fn post_comment(&self, intent: &PostCommentIntent) -> Result<String> {
        let site_id = intent.site_id.as_str();

        // 1. Ensure site space exists
        let space_id = self.ensure_site_space(site_id).await?;

        // 2. Determine the room alias based on the intent
        let homeserver = self.client.homeserver();
        let alias_localpart = format!("cumments_{}_{}", site_id, intent.post_slug.as_str());
        let room_alias_string = format!(
            "#cumments_{}_{}:{}",
            site_id,
            intent.post_slug.as_str(),
            homeserver.host().unwrap()
        );
        let room_alias: OwnedRoomAliasId = room_alias_string.as_str().try_into()?;

        info!("Looking for room with alias {}", room_alias);

        // 3. Try to resolve the alias to a room ID and join, or create it
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
            let room_name = format!("Comments: {}/{}", site_id, intent.post_slug.as_str());
            let topic = format!(
                "Comments for post '{}' on site '{}'",
                intent.post_slug.as_str(),
                site_id
            );

            let mut request = v3::Request::new();
            request.name = Some(room_name);
            request.topic = Some(topic);
            request.room_alias_name = Some(alias_localpart.try_into()?);
            request.preset = Some(RoomPreset::PublicChat);

            // [Dyarchy] Invite owner and grant Admin (PL 100)
            request.invite = vec![self.owner_id.clone()];

            let mut power_levels = RoomPowerLevelsEventContent::new(&AuthorizationRules::V1);
            let mut users = BTreeMap::new();
            if let Some(bot_id) = self.client.user_id() {
                users.insert(bot_id.to_owned(), Int::from(100));
            }
            users.insert(self.owner_id.clone(), Int::from(100));
            power_levels.users = users;

            let pl_event = InitialStateEvent::with_empty_state_key(power_levels);
            request.initial_state = vec![pl_event.to_raw_any()];

            let response = self.client.create_room(request).await?;
            let room_id = response.room_id().to_owned();
            info!("Successfully created and joined room {}", room_id);

            // 4. Link room to site space
            let _ = self.link_room_to_space(&room_id, &space_id).await;

            self.client
                .get_room(&room_id)
                .ok_or_else(|| anyhow!("Just-created room {} not found in client state", room_id))?
        };

        // 5. Post the message
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

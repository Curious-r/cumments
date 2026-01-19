use anyhow::Result;
use domain::SiteId;
use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    ruma::{
        api::client::room::create_room::v3::Request as CreateRoomRequest,
        api::client::room::create_room::v3::RoomPreset,
        api::client::state::get_state_events_for_key::v3::Request as GetStateRequest,
        events::{
            room::canonical_alias::RoomCanonicalAliasEventContent,
            space::child::SpaceChildEventContent, StateEventType, SyncStateEvent,
        },
        room::RoomType,
        serde::Raw,
        OwnedRoomId, RoomAliasId, ServerName,
    },
    Client, Room,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use tracing::{info, warn};

pub struct SpaceCache {
    inner: Arc<RwLock<HashMap<String, OwnedRoomId>>>,
}

impl SpaceCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

pub async fn resolve_room_alias_chain(room: &Room, client: &Client) -> Option<String> {
    if let Some(c) = room.canonical_alias() {
        return Some(c.to_string());
    }
    if let Some(alt) = room.alt_aliases().first() {
        return Some(alt.to_string());
    }

    if let Ok(Some(raw_state)) = room
        .get_state_event_static::<RoomCanonicalAliasEventContent>()
        .await
    {
        let alias_opt = match raw_state.deserialize() {
            Ok(SyncOrStrippedState::Sync(SyncStateEvent::Original(ev))) => ev.content.alias,
            Ok(SyncOrStrippedState::Stripped(ev)) => ev.content.alias,
            _ => None,
        };
        if let Some(a) = alias_opt {
            return Some(a.to_string());
        }
    }

    let req = GetStateRequest::new(
        room.room_id().to_owned(),
        StateEventType::RoomCanonicalAlias,
        "".to_string(),
    );

    if let Ok(response) = client.send(req, None).await {
        if let Ok(content) = response
            .content
            .deserialize_as::<RoomCanonicalAliasEventContent>()
        {
            if let Some(alias) = content.alias {
                info!(
                    "Fetched alias from Network API for room {}: {}",
                    room.room_id(),
                    alias
                );
                return Some(alias.to_string());
            }
        }
    }

    None
}

pub async fn create_and_link_room(
    client: &Client,
    server_name: &ServerName,
    space_id: &OwnedRoomId,
    site_id: &SiteId,
    slug: &str,
) -> Result<Room> {
    let alias_local = format!("{}_{}", site_id.as_str(), slug);
    let mut req = CreateRoomRequest::new();
    req.room_alias_name = Some(alias_local);
    req.name = Some(format!("Comments for {}", slug));
    req.preset = Some(RoomPreset::PublicChat);

    info!("Creating new room for slug: {}", slug);
    let room = client.create_room(req).await?;

    let space_room_opt = if let Some(r) = client.get_room(space_id) {
        Some(r)
    } else {
        client.join_room_by_id(space_id).await.ok()
    };

    if let Some(space_room) = space_room_opt {
        let server_name_owned = server_name.to_owned();
        let child = SpaceChildEventContent::new(vec![server_name_owned]);
        if let Err(e) = space_room
            .send_state_event_for_key(room.room_id(), child)
            .await
        {
            warn!("Failed to link room to space: {:?}", e);
        } else {
            info!("Linked new room {} to space", room.room_id());
        }
    }
    Ok(room)
}

pub async fn ensure_site_space(
    client: &Client,
    server_name: &ServerName,
    cache: &SpaceCache,
    site_id: &SiteId,
) -> Result<OwnedRoomId> {
    let site_id_str = site_id.as_str();
    {
        if let Some(id) = cache.inner.read().await.get(site_id_str) {
            return Ok(id.clone());
        }
    }

    let alias_local = format!("cumments_{}", site_id_str);
    let full_alias = format!("#{}:{}", alias_local, server_name);
    let alias = RoomAliasId::parse(&full_alias)?;

    let room_id = match client.resolve_room_alias(&alias).await {
        Ok(resp) => resp.room_id.to_owned(),
        Err(_) => {
            let mut cc =
                matrix_sdk::ruma::api::client::room::create_room::v3::CreationContent::new();
            cc.room_type = Some(RoomType::Space);
            let mut req = CreateRoomRequest::new();
            req.room_alias_name = Some(alias_local);
            req.name = Some(site_id_str.to_string());
            req.creation_content = Some(Raw::new(&cc)?);
            req.preset = Some(RoomPreset::PublicChat);

            let r = client.create_room(req).await?;
            r.room_id().to_owned()
        }
    };

    {
        cache
            .inner
            .write()
            .await
            .insert(site_id_str.to_string(), room_id.clone());
    }
    Ok(room_id)
}

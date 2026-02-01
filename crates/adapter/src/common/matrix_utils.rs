use anyhow::Result;
use domain::SiteId;
use matrix_sdk::{
    ruma::{
        api::client::alias::delete_alias::v3::Request as DeleteAliasRequest,
        api::client::room::create_room::v3::Request as CreateRoomRequest,
        api::client::room::create_room::v3::RoomPreset,
        events::{
            room::power_levels::RoomPowerLevelsEventContent,
            space::{child::SpaceChildEventContent, parent::SpaceParentEventContent}, // [新增] parent
        },
        room::RoomType,
        serde::Raw,
        OwnedRoomId, OwnedUserId, RoomAliasId, ServerName,
    },
    Client, Room, RoomState,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

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

async fn wait_for_room_ready(client: &Client, room_id: &OwnedRoomId) -> Option<Room> {
    for i in 0..20 {
        if let Some(r) = client.get_room(room_id) {
            if r.state() == RoomState::Joined {
                return Some(r);
            } else {
                debug!(
                    "Room {} found but state is {:?}, waiting...",
                    room_id,
                    r.state()
                );
            }
        } else {
            debug!("Room {} not found in local store yet, waiting...", room_id);
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        if i == 5 {
            let _ = client.join_room_by_id(room_id).await;
        }
    }
    None
}

fn build_create_request(
    alias_local: &str,
    name: &str,
    is_space: bool,
    owner_id: Option<&OwnedUserId>,
    bot_id: Option<OwnedUserId>,
) -> Result<CreateRoomRequest> {
    let mut req = CreateRoomRequest::new();
    req.room_alias_name = Some(alias_local.to_string());
    req.name = Some(name.to_string());

    if is_space {
        let mut cc = matrix_sdk::ruma::api::client::room::create_room::v3::CreationContent::new();
        cc.room_type = Some(RoomType::Space);
        req.creation_content = Some(Raw::new(&cc)?);
    } else {
        req.preset = Some(RoomPreset::PublicChat);
    }

    if let Some(owner) = owner_id {
        req.invite = vec![owner.clone()];
        let mut pl_content = RoomPowerLevelsEventContent::new();
        pl_content.users.insert(owner.clone(), 100.into());
        if let Some(bot) = bot_id {
            pl_content.users.insert(bot, 100.into());
        }
        req.power_level_content_override = Some(Raw::new(&pl_content)?);
    }

    Ok(req)
}
pub async fn resolve_room_alias_chain(room: &Room, client: &Client) -> Option<String> {
    use matrix_sdk::ruma::events::{
        room::canonical_alias::RoomCanonicalAliasEventContent, StateEventType,
    };
    if let Some(c) = room.canonical_alias() {
        return Some(c.to_string());
    }
    if let Some(alt) = room.alt_aliases().first() {
        return Some(alt.to_string());
    }

    let req = matrix_sdk::ruma::api::client::state::get_state_events_for_key::v3::Request::new(
        room.room_id().to_owned(),
        StateEventType::RoomCanonicalAlias,
        "".to_string(),
    );
    if let Ok(res) = client.send(req, None).await {
        if let Ok(content) = res
            .content
            .deserialize_as::<RoomCanonicalAliasEventContent>()
        {
            if let Some(alias) = content.alias {
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
    owner_id: Option<&OwnedUserId>,
) -> Result<Room> {
    let alias_local = format!("{}_{}", site_id.as_str(), slug);
    let full_alias_str = format!("#{}:{}", alias_local, server_name);

    let req = build_create_request(
        &alias_local,
        &format!("Comments for {}", slug),
        false,
        owner_id,
        client.user_id().map(|u| u.to_owned()),
    )?;

    let room_response = match client.create_room(req).await {
        Ok(r) => r,
        Err(e) => {
            if let Ok(alias) = RoomAliasId::parse(&full_alias_str) {
                warn!("Room creation failed, checking for orphan alias: {}", alias);
                match client.resolve_room_alias(&alias).await {
                    Ok(_) => {
                        info!("Deleting orphan alias: {}", alias);
                        let del_req = DeleteAliasRequest::new(alias);
                        let _ = client.send(del_req, None).await;
                        info!("Orphan alias deleted, retrying creation...");
                        let req_retry = build_create_request(
                            &alias_local,
                            &format!("Comments for {}", slug),
                            false,
                            owner_id,
                            client.user_id().map(|u| u.to_owned()),
                        )?;
                        client.create_room(req_retry).await?
                    }
                    Err(_) => return Err(e.into()),
                }
            } else {
                return Err(e.into());
            }
        }
    };

    let child_id = room_response.room_id().to_owned();

    if let Some(space_room) = wait_for_room_ready(client, space_id).await {
        info!("Space {} is ready. Linking child {}...", space_id, child_id);

        let server_name_owned = server_name.to_owned();

        let child_content = SpaceChildEventContent::new(vec![server_name_owned.clone()]);
        if let Err(e) = space_room
            .send_state_event_for_key(&child_id, child_content)
            .await
        {
            error!("Failed to link room to space (Downlink): {:?}", e);
        } else {
            info!("Linked child {} to space {}", child_id, space_id);
        }

        if let Some(child_room) = wait_for_room_ready(client, &child_id).await {
            let parent_content = SpaceParentEventContent::new(vec![server_name_owned]);
            if let Err(e) = child_room
                .send_state_event_for_key(space_id, parent_content)
                .await
            {
                warn!("Failed to set space parent (Uplink): {:?}", e);
            }
        }
    } else {
        error!(
            "CRITICAL: Space room {} not found after waiting. Child {} created ORPHANED.",
            space_id, child_id
        );
    }

    let room = client
        .get_room(&child_id)
        .ok_or_else(|| anyhow::anyhow!("Created room but SDK cannot find it"))?;

    Ok(room)
}
pub async fn ensure_site_space(
    client: &Client,
    server_name: &ServerName,
    cache: &SpaceCache,
    site_id: &SiteId,
    owner_id: Option<&OwnedUserId>,
) -> Result<OwnedRoomId> {
    let site_id_str = site_id.as_str();

    {
        if let Some(id) = cache.inner.read().await.get(site_id_str) {
            if let Some(r) = client.get_room(id) {
                if r.state() == RoomState::Joined {
                    return Ok(id.clone());
                }
                info!(
                    "Cached space {} state is {:?}. Revalidating...",
                    id,
                    r.state()
                );
            }
        }
    }

    let alias_local = format!("cumments_{}", site_id_str);
    let full_alias = format!("#{}:{}", alias_local, server_name);
    let alias = RoomAliasId::parse(&full_alias)?;

    let room_id = match client.resolve_room_alias(&alias).await {
        Ok(resp) => {
            let rid = resp.room_id;
            let state = client.get_room(&rid).map(|r| r.state());

            match state {
                Some(RoomState::Joined) => rid,
                Some(RoomState::Left) | Some(RoomState::Invited) => {
                    info!(
                        "Space alias {} points to Left/Invited room. Recreating...",
                        alias
                    );
                    let del_req = DeleteAliasRequest::new(alias.clone());
                    let _ = client.send(del_req, None).await;

                    let req = build_create_request(
                        &alias_local,
                        site_id_str,
                        true,
                        owner_id,
                        client.user_id().map(|u| u.to_owned()),
                    )?;
                    let r = client.create_room(req).await?;
                    r.room_id().to_owned()
                }
                None => match client.join_room_by_id(&rid).await {
                    Ok(_) => rid,
                    Err(e) => {
                        info!("Failed to join existing space ({:?}). Recreating...", e);
                        let del_req = DeleteAliasRequest::new(alias.clone());
                        let _ = client.send(del_req, None).await;

                        let req = build_create_request(
                            &alias_local,
                            site_id_str,
                            true,
                            owner_id,
                            client.user_id().map(|u| u.to_owned()),
                        )?;
                        let r = client.create_room(req).await?;
                        r.room_id().to_owned()
                    }
                },
            }
        }
        Err(_) => {
            let req = build_create_request(
                &alias_local,
                site_id_str,
                true,
                owner_id,
                client.user_id().map(|u| u.to_owned()),
            )?;
            let r = client.create_room(req).await?;
            r.room_id().to_owned()
        }
    };

    if wait_for_room_ready(client, &room_id).await.is_none() {
        warn!("Space created/joined but not ready in local store yet.");
    }

    {
        cache
            .inner
            .write()
            .await
            .insert(site_id_str.to_string(), room_id.clone());
    }

    Ok(room_id)
}

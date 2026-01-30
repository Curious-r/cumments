use crate::common::matrix_utils::{create_and_link_room, SpaceCache};
use crate::AppServiceConfig;
use anyhow::Result;
use domain::SiteId;
use matrix_sdk::{
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    ruma::{OwnedRoomId, OwnedUserId, RoomAliasId, ServerName, UserId},
    Client, SessionMeta,
};

pub async fn get_ghost_client(config: &AppServiceConfig, user_id: &UserId) -> Result<Client> {
    let client = Client::builder()
        .homeserver_url(&config.homeserver_url)
        .build()
        .await?;

    let session = MatrixSession {
        meta: SessionMeta {
            user_id: user_id.to_owned(),
            device_id: "AS_GHOST".into(),
        },
        tokens: MatrixSessionTokens {
            access_token: config.as_token.clone(),
            refresh_token: None,
        },
    };

    client.matrix_auth().restore_session(session).await?;
    Ok(client)
}

pub async fn ensure_room_for_as(
    client: &Client,
    config: &AppServiceConfig,
    cache: &SpaceCache,
    site_id: &SiteId,
    slug: &str,
    owner_id: Option<&OwnedUserId>,
) -> Result<OwnedRoomId> {
    let full_alias = format!("#{}_{}:{}", site_id.as_str(), slug, config.server_name);
    let room_alias = RoomAliasId::parse(&full_alias)?;

    if let Ok(resp) = client.resolve_room_alias(&room_alias).await {
        return Ok(resp.room_id);
    }

    let space_id = crate::common::matrix_utils::ensure_site_space(
        client,
        &ServerName::parse(&config.server_name)?,
        cache,
        site_id,
    )
    .await?;

    let room = create_and_link_room(
        client,
        &ServerName::parse(&config.server_name)?,
        &space_id,
        site_id,
        slug,
        owner_id,
    )
    .await?;

    Ok(room.room_id().to_owned())
}

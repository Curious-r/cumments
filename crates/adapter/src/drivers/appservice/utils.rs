use crate::common::matrix_utils::{create_and_link_room, SpaceCache};
use crate::AppServiceConfig;
use anyhow::Result;
use domain::SiteId;
use matrix_sdk::{
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    ruma::{OwnedRoomId, OwnedUserId, RoomAliasId, ServerName, UserId},
    Client, SessionMeta,
};

/// 获取或创建 Ghost 用户的 Client
pub async fn get_ghost_client(config: &AppServiceConfig, user_id: &UserId) -> Result<Client> {
    // 注意：在生产环境中，Client 构建成本较高，
    // 理想情况下应该有一个 LRU Cache 来缓存 Ghost Client。
    // 但鉴于 matrix-sdk Client 本身较重，这里先按需创建，
    // 后续优化可以考虑只缓存 HttpService。

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
            refresh_token: None, // AS 代表用户，不需要刷新 Token
        },
    };

    client.matrix_auth().restore_session(session).await?;
    Ok(client)
}

/// 确保房间存在 (支持双皇共治)
pub async fn ensure_room_for_as(
    client: &Client,
    config: &AppServiceConfig,
    cache: &SpaceCache,
    site_id: &SiteId,
    slug: &str,
    owner_id: Option<&OwnedUserId>, // 透传 Owner
) -> Result<OwnedRoomId> {
    let full_alias = format!("#{}_{}:{}", site_id.as_str(), slug, config.server_name);
    let room_alias = RoomAliasId::parse(&full_alias)?;

    // 1. 尝试解析别名
    if let Ok(resp) = client.resolve_room_alias(&room_alias).await {
        return Ok(resp.room_id);
    }

    // 2. 确保 Space 存在
    let space_id = crate::common::matrix_utils::ensure_site_space(
        client,
        &ServerName::parse(&config.server_name)?,
        cache,
        site_id,
    )
    .await?;

    // 3. 创建房间 (包含 Owner Invite 和 PowerLevel 设置)
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

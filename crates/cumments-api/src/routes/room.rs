//! Room metadata and system-message endpoint.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use crate::routes::media::media_url_base;
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::HeaderMap,
};
use cumments_core::models::{PageSlug, RoomMetadata, SiteId};
use serde::Serialize;

#[derive(Serialize)]
struct RoomInfoResponse {
    room_id: String,
    name: Option<String>,
    topic: Option<String>,
    avatar_url: Option<String>,
    avatar_thumbnail_url: Option<String>,
    member_count: i64,
    system_messages: Vec<cumments_core::models::RoomStateEvent>,
}

/// `GET /api/v1/sites/{site_id}/pages/{page_slug}/room`
pub(crate) async fn room_info_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path((site_id, page_slug)): Path<(String, String)>,
) -> Result<impl axum::response::IntoResponse, AppError> {
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.public_read_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "public reads are rate limited; try again later".to_string(),
            retry_after_seconds: state.public_read_limiter.window().as_secs(),
        });
    }
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug).map_err(AppError::Validation)?;

    let Some(room_id) = state
        .store
        .get_registered_room(&site_id_val, &page_slug_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve room: {e}")))?
    else {
        return Err(AppError::NotFound(
            "No room registered for this post.".to_string(),
        ));
    };

    let mut metadata = state
        .store
        .get_room_metadata(&room_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read room metadata: {e}")))?
        .unwrap_or(RoomMetadata {
            room_id: room_id.clone(),
            name: None,
            topic: None,
            avatar_url: None,
            avatar_thumbnail_url: None,
            member_count: 0,
        });
    let media_base = media_url_base(&state, &headers, Some(addr));
    if let Some(proxy) = &state.media_proxy
        && let Some(avatar) = metadata
            .avatar_url
            .as_deref()
            .and_then(|url| proxy.proxify(url, &media_base))
    {
        metadata.avatar_url = Some(avatar);
    }
    if let Some(proxy) = &state.media_proxy
        && let Some(avatar) = metadata
            .avatar_thumbnail_url
            .as_deref()
            .and_then(|url| proxy.proxify_avatar(url, &media_base))
    {
        metadata.avatar_thumbnail_url = Some(avatar);
    }
    let system_messages = state
        .store
        .get_room_system_messages(&room_id, 20)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read system messages: {e}")))?;

    Ok(Json(RoomInfoResponse {
        room_id,
        name: metadata.name,
        topic: metadata.topic,
        avatar_url: metadata.avatar_url,
        avatar_thumbnail_url: metadata.avatar_thumbnail_url,
        member_count: metadata.member_count,
        system_messages,
    }))
}

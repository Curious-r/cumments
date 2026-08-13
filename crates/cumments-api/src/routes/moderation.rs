//! Site governance endpoints: owners, co-managers and room moderators.
//!
//! Writes are authenticated with the site's claim token (or the admin token
//! on the admin-router mirror) and go straight to Matrix: the backend makes
//! the AS sender update `m.room.power_levels`, and the normal push projection
//! brings the change back into the read model. The role lists returned by the
//! read endpoints come from that projection.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use axum::{
    Json,
    extract::{ConnectInfo, Path, Request, State},
    http::HeaderMap,
    middleware::Next,
    response::{IntoResponse, Response},
};
use cumments_core::{
    governance::{
        CO_MANAGER_LEVEL, MODERATOR_LEVEL, OWNER_LEVEL, RoleEntry, is_as_managed_user,
        role_entries, with_user_level, without_user,
    },
    models::{PostSlug, SiteId},
    site_auth::{CLAIM_TOKEN_HEADER, constant_time_eq, token_hash},
};
use ruma_common::UserId;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Debug, Deserialize)]
pub struct UserIdRequest {
    pub user_id: String,
}

#[derive(Debug, Serialize)]
pub struct SiteRolesResponse {
    pub owners: Vec<String>,
    pub co_managers: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RoomModeratorsResponse {
    pub room_id: String,
    pub moderators: Vec<String>,
}

/// Authenticates governance writes with the site's claim token.
pub async fn require_claim_token(
    State(state): State<ApiState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(site_id) = crate::site_auth::site_id_from_path(req.uri().path()) else {
        return AppError::Unauthorized("missing site id in path".to_string()).into_response();
    };
    match verify_claim_token(&state, &site_id, req.headers()).await {
        Ok(()) => next.run(req).await,
        Err(error) => error.into_response(),
    }
}

async fn verify_claim_token(
    state: &ApiState,
    site_id: &str,
    headers: &HeaderMap,
) -> Result<(), AppError> {
    let presented = headers
        .get(CLAIM_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| AppError::Unauthorized(format!("missing {CLAIM_TOKEN_HEADER} header")))?;
    let stored = state
        .store
        .get_claim_token_hash(site_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load claim token: {e}")))?
        .ok_or_else(|| {
            AppError::Unauthorized(
                "site is not API-registered; operator-configured sites are managed \
                 through configuration"
                    .to_string(),
            )
        })?;
    if !constant_time_eq(stored.as_bytes(), token_hash(presented).as_bytes()) {
        return Err(AppError::Unauthorized("invalid claim token".to_string()));
    }
    Ok(())
}

/// Validates a Matrix user ID and rejects Cumments service accounts.
fn parse_user_id(raw: &str) -> Result<String, AppError> {
    let parsed = UserId::parse(raw)
        .map_err(|_| AppError::BadRequest("invalid Matrix user ID".to_string()))?;
    let user_id = parsed.as_str().to_string();
    if is_as_managed_user(&user_id) {
        return Err(AppError::BadRequest(
            "Cumments service accounts cannot hold governance roles".to_string(),
        ));
    }
    Ok(user_id)
}

fn rate_limited(
    state: &ApiState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
) -> Result<(), AppError> {
    let key = client_key(headers, addr, &state.trusted_proxies);
    if !state.moderation_limiter.allow(&key) {
        return Err(AppError::TooManyRequests(
            "site governance writes are rate limited; try again later".to_string(),
        ));
    }
    Ok(())
}

async fn site_space_id(state: &ApiState, site_id: &SiteId) -> Result<String, AppError> {
    // Ensures the Space exists, provisioning it on first use so the owner can
    // bootstrap before any comment has been posted.
    state
        .site_service
        .ensure_space(site_id, state.driver.as_ref())
        .await
        .map_err(|e| AppError::Internal(format!("failed to provision site space: {e}")))
}

fn site_roles_response(roles: Vec<RoleEntry>) -> SiteRolesResponse {
    SiteRolesResponse {
        owners: roles
            .iter()
            .filter(|role| role.level == OWNER_LEVEL)
            .map(|role| role.user_id.clone())
            .collect(),
        co_managers: roles
            .iter()
            .filter(|role| role.level == CO_MANAGER_LEVEL)
            .map(|role| role.user_id.clone())
            .collect(),
    }
}

async fn set_space_role(
    state: &ApiState,
    site_id: &SiteId,
    user_id: &str,
    level: i64,
    add: bool,
) -> Result<SiteRolesResponse, AppError> {
    let space_id = site_space_id(state, site_id).await?;
    let current = state
        .driver
        .get_room_power_levels(&space_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read space power levels: {e}")))?
        .unwrap_or_else(|| serde_json::json!({}));
    let updated = if add {
        with_user_level(&current, user_id, level)
    } else {
        without_user(&current, user_id)
    };
    state
        .driver
        .set_room_power_levels(&space_id, &updated)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update space power levels: {e}")))?;
    if add {
        state
            .driver
            .invite_user(&space_id, user_id)
            .await
            .map_err(|e| AppError::Internal(format!("failed to invite {user_id}: {e}")))?;
    }
    // The moderation sync pass replicates the change into every comment room.
    state.reconciler_notify.notify_one();

    let roles = role_entries(&updated, MODERATOR_LEVEL)
        .into_iter()
        .filter(|role| !is_as_managed_user(&role.user_id))
        .collect();
    Ok(site_roles_response(roles))
}

pub(crate) async fn add_owner_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<SiteRolesResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    set_space_role(&state, &site_id, &user_id, OWNER_LEVEL, true)
        .await
        .map(Json)
}

pub(crate) async fn remove_owner_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<SiteRolesResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    set_space_role(&state, &site_id, &user_id, OWNER_LEVEL, false)
        .await
        .map(Json)
}

pub(crate) async fn add_co_manager_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<SiteRolesResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    set_space_role(&state, &site_id, &user_id, CO_MANAGER_LEVEL, true)
        .await
        .map(Json)
}

pub(crate) async fn remove_co_manager_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<SiteRolesResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    set_space_role(&state, &site_id, &user_id, CO_MANAGER_LEVEL, false)
        .await
        .map(Json)
}

async fn set_room_moderator(
    state: &ApiState,
    site_id: &SiteId,
    post_slug: &PostSlug,
    user_id: &str,
    add: bool,
) -> Result<RoomModeratorsResponse, AppError> {
    let room_id = state
        .store
        .get_registered_room(site_id, post_slug)
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve room: {e}")))?
        .ok_or_else(|| AppError::NotFound("No room registered for this post.".to_string()))?;
    let current = state
        .driver
        .get_room_power_levels(&room_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read room power levels: {e}")))?
        .unwrap_or_else(|| serde_json::json!({}));
    // Site-level roles (owner/co-manager) are managed on the Space and
    // replicated here by the sync pass; the room-moderator endpoint must not
    // fight that reconciliation.
    let current_level = role_entries(&current, MODERATOR_LEVEL)
        .into_iter()
        .find(|role| role.user_id == user_id)
        .map(|role| role.level)
        .unwrap_or(0);
    if current_level >= CO_MANAGER_LEVEL {
        return Err(AppError::BadRequest(
            "this user holds a site-level role; manage them through the owners \
             or co-managers endpoint"
                .to_string(),
        ));
    }
    let updated = if add {
        with_user_level(&current, user_id, MODERATOR_LEVEL)
    } else {
        without_user(&current, user_id)
    };
    state
        .driver
        .set_room_power_levels(&room_id, &updated)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update room power levels: {e}")))?;
    if add {
        state
            .driver
            .invite_user(&room_id, user_id)
            .await
            .map_err(|e| AppError::Internal(format!("failed to invite {user_id}: {e}")))?;
    }

    let moderators = role_entries(&updated, MODERATOR_LEVEL)
        .into_iter()
        .filter(|role| role.level == MODERATOR_LEVEL && !is_as_managed_user(&role.user_id))
        .map(|role| role.user_id)
        .collect();
    Ok(RoomModeratorsResponse {
        room_id,
        moderators,
    })
}

pub(crate) async fn add_room_moderator_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<RoomModeratorsResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug = PostSlug::new(post_slug).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    set_room_moderator(&state, &site_id, &post_slug, &user_id, true)
        .await
        .map(Json)
}

pub(crate) async fn remove_room_moderator_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<RoomModeratorsResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug = PostSlug::new(post_slug).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    set_room_moderator(&state, &site_id, &post_slug, &user_id, false)
        .await
        .map(Json)
}

pub(crate) async fn list_site_roles_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
) -> Result<Json<SiteRolesResponse>, AppError> {
    let roles = state
        .store
        .list_site_roles(&site_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list site roles: {e}")))?;
    Ok(Json(site_roles_response(roles)))
}

pub(crate) async fn list_room_moderators_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
) -> Result<Json<RoomModeratorsResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug = PostSlug::new(post_slug).map_err(AppError::Validation)?;
    let room_id = state
        .store
        .get_registered_room(&site_id, &post_slug)
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve room: {e}")))?
        .ok_or_else(|| AppError::NotFound("No room registered for this post.".to_string()))?;
    let moderators = state
        .store
        .list_room_roles(&room_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list room roles: {e}")))?
        .into_iter()
        .filter(|role| role.level == MODERATOR_LEVEL)
        .map(|role| role.user_id)
        .collect();
    Ok(Json(RoomModeratorsResponse {
        room_id,
        moderators,
    }))
}

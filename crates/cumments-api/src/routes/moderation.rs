//! Site governance endpoints: owners, co-managers and room moderators.
//!
//! Role registration is always token-DM verified: the API stores a pending
//! claim and returns a one-time token; the target MXID sends the token to the
//! AS bot, and the reconciler then writes the role to Matrix power levels.
//! Revocations remove either the pending claim or the applied Matrix role.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::HeaderMap,
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use cumments_core::{
    governance::{
        CO_MANAGER_LEVEL, MODERATOR_LEVEL, NewRoleClaim, OWNER_LEVEL, RoleEntry, role_entries,
        set_role_level, validate_governance_user_id,
    },
    models::{PostSlug, SiteId},
    site_auth::{CLAIM_TOKEN_HEADER, constant_time_eq, generate_token, token_hash},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;

/// How long an unverified role claim stays valid.
const CLAIM_TTL_HOURS: i64 = 24;

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

#[derive(Debug, Serialize)]
pub struct PendingRoleResponse {
    pub pending: bool,
    pub user_id: String,
    pub level: i64,
    pub verify_token: String,
    pub expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct RevokedRoleResponse {
    pub revoked: bool,
    pub user_id: String,
    pub level: i64,
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
    validate_governance_user_id(raw).map_err(|error| AppError::BadRequest(error.to_string()))
}

/// Reads the mandatory `user_id` from a DELETE query string with a
/// problem-details error instead of axum's default rejection body.
fn user_id_from_query(query: &HashMap<String, String>) -> Result<String, AppError> {
    query
        .get("user_id")
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| AppError::BadRequest("user_id query parameter is required".to_string()))
}

fn rate_limited(
    state: &ApiState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
) -> Result<(), AppError> {
    let key = client_key(headers, addr, &state.trusted_proxies);
    if !state.moderation_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "site governance writes are rate limited; try again later".to_string(),
            retry_after_seconds: state.moderation_limiter.window().as_secs(),
        });
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

async fn room_id_for(
    state: &ApiState,
    site_id: &SiteId,
    post_slug: &PostSlug,
) -> Result<String, AppError> {
    state
        .store
        .get_registered_room(site_id, post_slug)
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve room: {e}")))?
        .ok_or_else(|| AppError::NotFound("No room registered for this post.".to_string()))
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

/// Creates (or rotates) a pending token-DM claim for a role.
async fn create_role_claim(
    state: &ApiState,
    site_id: &SiteId,
    room_id: &str,
    user_id: &str,
    level: i64,
) -> Result<PendingRoleResponse, AppError> {
    let verify_token = generate_token();
    let claim = state
        .store
        .upsert_role_claim(&NewRoleClaim {
            site_id: site_id.as_str().to_string(),
            room_id: room_id.to_string(),
            user_id: user_id.to_string(),
            level,
            token_hash: token_hash(&verify_token),
            expires_at: Utc::now() + Duration::hours(CLAIM_TTL_HOURS),
        })
        .await
        .map_err(|e| AppError::Internal(format!("failed to store role claim: {e}")))?;
    Ok(PendingRoleResponse {
        pending: true,
        user_id: user_id.to_string(),
        level,
        verify_token,
        expires_at: claim.expires_at,
    })
}

/// Removes an already-applied site-level role from the Space. The moderation
/// sync then propagates the removal into every comment room.
async fn remove_site_role(
    state: &ApiState,
    site_id: &SiteId,
    user_id: &str,
    level: i64,
) -> Result<(), AppError> {
    let space_id = site_space_id(state, site_id).await?;
    set_role_level(state.driver.as_ref(), &space_id, user_id, level, false)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update space power levels: {e}")))?;
    state.reconciler_notify.notify_one();
    Ok(())
}

/// Removes an already-applied room moderator. Site-level roles are rejected so
/// this endpoint cannot fight the moderation sync pass.
async fn remove_room_moderator_role(
    state: &ApiState,
    site_id: &SiteId,
    post_slug: &PostSlug,
    user_id: &str,
) -> Result<(), AppError> {
    let room_id = room_id_for(state, site_id, post_slug).await?;
    let current = state
        .driver
        .get_room_power_levels(&room_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read room power levels: {e}")))?
        .unwrap_or_else(|| serde_json::json!({}));
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
    set_role_level(
        state.driver.as_ref(),
        &room_id,
        user_id,
        MODERATOR_LEVEL,
        false,
    )
    .await
    .map_err(|e| AppError::Internal(format!("failed to update room power levels: {e}")))?;
    Ok(())
}

pub(crate) async fn add_owner_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<PendingRoleResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    create_role_claim(&state, &site_id, "", &user_id, OWNER_LEVEL)
        .await
        .map(Json)
}

pub(crate) async fn remove_owner_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<RevokedRoleResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&user_id_from_query(&query)?)?;
    let revoked = state
        .store
        .revoke_role_claim(site_id.as_str(), "", &user_id, OWNER_LEVEL)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke role claim: {e}")))?;
    if !revoked {
        remove_site_role(&state, &site_id, &user_id, OWNER_LEVEL).await?;
    }
    Ok(Json(RevokedRoleResponse {
        revoked: true,
        user_id,
        level: OWNER_LEVEL,
    }))
}

pub(crate) async fn add_co_manager_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<PendingRoleResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    create_role_claim(&state, &site_id, "", &user_id, CO_MANAGER_LEVEL)
        .await
        .map(Json)
}

pub(crate) async fn remove_co_manager_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<RevokedRoleResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&user_id_from_query(&query)?)?;
    let revoked = state
        .store
        .revoke_role_claim(site_id.as_str(), "", &user_id, CO_MANAGER_LEVEL)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke role claim: {e}")))?;
    if !revoked {
        remove_site_role(&state, &site_id, &user_id, CO_MANAGER_LEVEL).await?;
    }
    Ok(Json(RevokedRoleResponse {
        revoked: true,
        user_id,
        level: CO_MANAGER_LEVEL,
    }))
}

pub(crate) async fn add_room_moderator_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UserIdRequest>,
) -> Result<Json<PendingRoleResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug = PostSlug::new(post_slug).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&req.user_id)?;
    let room_id = room_id_for(&state, &site_id, &post_slug).await?;
    create_role_claim(&state, &site_id, &room_id, &user_id, MODERATOR_LEVEL)
        .await
        .map(Json)
}

pub(crate) async fn remove_room_moderator_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<RevokedRoleResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug = PostSlug::new(post_slug).map_err(AppError::Validation)?;
    rate_limited(&state, &headers, Some(connect.0))?;
    let user_id = parse_user_id(&user_id_from_query(&query)?)?;
    let room_id = room_id_for(&state, &site_id, &post_slug).await?;
    let revoked = state
        .store
        .revoke_role_claim(site_id.as_str(), &room_id, &user_id, MODERATOR_LEVEL)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke role claim: {e}")))?;
    if !revoked {
        remove_room_moderator_role(&state, &site_id, &post_slug, &user_id).await?;
    }
    Ok(Json(RevokedRoleResponse {
        revoked: true,
        user_id,
        level: MODERATOR_LEVEL,
    }))
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
    let room_id = room_id_for(&state, &site_id, &post_slug).await?;
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

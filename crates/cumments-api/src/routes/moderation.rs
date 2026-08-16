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
use chrono::Utc;
use cumments_core::{
    governance::{
        CO_MANAGER_LEVEL, MODERATOR_LEVEL, OWNER_LEVEL, RoleEntry, validate_governance_user_id,
    },
    management::ManagementError,
    models::{PostSlug, SiteId},
    site_auth::{CLAIM_TOKEN_HEADER, constant_time_eq, token_hash},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

#[derive(Debug, Deserialize)]
pub struct UpgradePostRoomRequest {
    pub new_version: String,
}

#[derive(Debug, Serialize)]
pub struct UpgradePostRoomResponse {
    pub site_id: String,
    pub post_slug: String,
    pub new_version: String,
    pub replacement_room: String,
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
    /// Non-empty only when the revocation leaves the site in a notable state
    /// (e.g. the last owner was removed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RetireSiteResponse {
    pub site_id: String,
    pub status: &'static str,
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

pub(crate) fn rate_limited(
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

/// Maps shared management failures onto HTTP problem responses. Infra errors
/// stay internal; the remaining variants are caller mistakes or state
/// conflicts that deserve a 4xx status.
fn map_management_error(error: ManagementError) -> AppError {
    match error {
        ManagementError::InvalidUserId(message) | ManagementError::InvalidSiteId(message) => {
            AppError::BadRequest(message)
        }
        ManagementError::InvalidRoomVersion(message)
        | ManagementError::InvalidPostSlug(message) => AppError::BadRequest(message),
        ManagementError::RoomVersionNotNewer(message, _) => AppError::Conflict(message),
        ManagementError::RoomWithoutCreateEvent(message) => AppError::NotFound(message),
        ManagementError::RoleNotFound => AppError::NotFound(error.to_string()),
        ManagementError::SiteLevelRoleConflict => AppError::Conflict(error.to_string()),
        ManagementError::RoomNotRegistered(message) => AppError::NotFound(message),
        ManagementError::RoomNotActive(message) => AppError::Conflict(message),
        ManagementError::Infra(error) => {
            AppError::Internal(format!("management operation failed: {error}"))
        }
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
    let pending = cumments_core::management::create_role_claim(
        state.store.as_ref(),
        site_id.as_str(),
        room_id,
        user_id,
        level,
    )
    .await
    .map_err(map_management_error)?;
    Ok(PendingRoleResponse {
        pending: true,
        user_id: pending.user_id,
        level,
        verify_token: pending.verify_token,
        expires_at: pending.expires_at,
    })
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
    let removal = cumments_core::management::remove_site_role(
        state.store.as_ref(),
        state.store.as_ref(),
        state.driver.as_ref(),
        &state.site_service,
        site_id.as_str(),
        &user_id,
        OWNER_LEVEL,
    )
    .await
    .map_err(map_management_error)?;
    if removal == cumments_core::management::RoleRemoval::AppliedRemoved {
        state.governance_notify.notify_one();
    }
    let mut warnings = Vec::new();
    let roles = state
        .store
        .list_site_roles(site_id.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to list site roles: {e}")))?;
    warnings.extend(owner_removal_warnings(&roles));
    Ok(Json(RevokedRoleResponse {
        revoked: true,
        user_id,
        level: OWNER_LEVEL,
        warnings,
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
    let removal = cumments_core::management::remove_site_role(
        state.store.as_ref(),
        state.store.as_ref(),
        state.driver.as_ref(),
        &state.site_service,
        site_id.as_str(),
        &user_id,
        CO_MANAGER_LEVEL,
    )
    .await
    .map_err(map_management_error)?;
    if removal == cumments_core::management::RoleRemoval::AppliedRemoved {
        state.governance_notify.notify_one();
    }
    Ok(Json(RevokedRoleResponse {
        revoked: true,
        user_id,
        level: CO_MANAGER_LEVEL,
        warnings: Vec::new(),
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
    let removal = cumments_core::management::remove_room_moderator(
        state.store.as_ref(),
        state.store.as_ref(),
        state.driver.as_ref(),
        site_id.as_str(),
        &room_id,
        &user_id,
    )
    .await
    .map_err(map_management_error)?;
    if removal == cumments_core::management::RoleRemoval::AppliedRemoved {
        state.governance_notify.notify_one();
    }
    Ok(Json(RevokedRoleResponse {
        revoked: true,
        user_id,
        level: MODERATOR_LEVEL,
        warnings: Vec::new(),
    }))
}

pub(crate) async fn list_site_roles_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
) -> Result<Json<SiteRolesResponse>, AppError> {
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.public_read_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "public reads are rate limited; try again later".to_string(),
            retry_after_seconds: state.public_read_limiter.window().as_secs(),
        });
    }
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    if state
        .store
        .get_site(&site_id_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up site: {e}")))?
        .is_none()
    {
        return Err(AppError::NotFound("Site not found.".to_string()));
    }
    let roles = state
        .store
        .list_site_roles(site_id_val.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to list site roles: {e}")))?;
    Ok(Json(site_roles_response(roles)))
}

pub(crate) async fn list_room_moderators_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((site_id, post_slug)): Path<(String, String)>,
) -> Result<Json<RoomModeratorsResponse>, AppError> {
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.public_read_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "public reads are rate limited; try again later".to_string(),
            retry_after_seconds: state.public_read_limiter.window().as_secs(),
        });
    }
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

/// Starts site decommission: writes are rejected immediately, then the
/// reconciler retires the Matrix Space/rooms and clears local projections.
pub(crate) async fn retire_site_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
) -> Result<Json<RetireSiteResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let marked = cumments_core::management::retire_site(state.store.as_ref(), site_id.as_str())
        .await
        .map_err(map_management_error)?;
    if !marked {
        return Err(AppError::NotFound(
            "site not found or already decommissioned".to_string(),
        ));
    }
    state.governance_notify.notify_one();
    Ok(Json(RetireSiteResponse {
        site_id: site_id.as_str().to_string(),
        status: "retiring",
    }))
}

/// Site-owner entry point for upgrading one of the site's comment rooms.
///
/// The room is resolved from the registry by `(site_id, post_slug)`, so the
/// claim token's site scope is the whole authorization boundary. The upgrade
/// itself is executed by the AS bot (the `/upgrade` caller), keeping the bot
/// as the replacement room's creator; the operator mirror accepts a raw
/// room ID and lives under `/api/v1/operator/rooms/{room_id}/upgrade`.
pub(crate) async fn upgrade_post_room_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    Json(body): Json<UpgradePostRoomRequest>,
) -> Result<Json<UpgradePostRoomResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug = PostSlug::new(post_slug).map_err(AppError::Validation)?;
    let replacement_room = cumments_core::management::upgrade_site_post_room(
        state.driver.as_ref(),
        state.store.as_ref(),
        &state.site_service,
        &site_id,
        &post_slug,
        &body.new_version,
    )
    .await
    .map_err(map_management_error)?;
    Ok(Json(UpgradePostRoomResponse {
        site_id: site_id.as_str().to_string(),
        post_slug: post_slug.as_str().to_string(),
        new_version: body.new_version,
        replacement_room,
    }))
}

/// Warnings attached to a role-revocation response. Currently only the
/// "last site owner" case is notable: the site stays operational because the
/// AppService sender remains the backstop, but no human can manage it.
fn owner_removal_warnings(roles: &[RoleEntry]) -> Vec<String> {
    if roles.iter().any(|role| role.level == OWNER_LEVEL) {
        Vec::new()
    } else {
        vec![
            "last site owner revoked; the site has no human owner and the AppService \
             sender remains the only backstop"
                .to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_removal_warns_only_when_no_owner_remains() {
        assert!(owner_removal_warnings(&[]).len() == 1);
        assert!(
            owner_removal_warnings(&[RoleEntry {
                user_id: "@co:hs".into(),
                level: CO_MANAGER_LEVEL,
            }])
            .len()
                == 1
        );
        assert!(
            owner_removal_warnings(&[RoleEntry {
                user_id: "@owner:hs".into(),
                level: OWNER_LEVEL,
            }])
            .is_empty()
        );
    }
}

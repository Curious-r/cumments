//! Operator-only operator routes for database-tracked sites.
//!
//! Protected by a static bearer token (`security.operator_token`). Configuration
//! remains the operator's declarative surface: operator endpoints never write
//! config files, they manage runtime state and print adoption snippets.

use crate::ApiState;
use crate::error::{AppError, map_management_error};
use crate::rate_limit::client_key;
use crate::routes::comments::{ACCEPT_QUERY, QUERY_METHOD};
use axum::extract::Request;
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{Method, StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cumments_core::models::SiteId;
use cumments_core::operator::{
    OperatorListQuery, OperatorSite, UpgradeIntentListQuery, config_snippet_toml,
    list_operator_quarantined_rooms, list_operator_sites, list_operator_upgrade_intents,
    operator_site,
};
use cumments_core::site_auth::{SiteAuthMode, constant_time_eq, generate_token, token_hash};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct RevokeOriginRequest {
    pub origin: String,
}

#[derive(Debug, Serialize)]
pub struct RotateSecretResponse {
    pub site_id: String,
    pub secret: String,
}

#[derive(Debug, Serialize)]
pub struct RevokeSecretResponse {
    pub site_id: String,
    pub auth_mode: SiteAuthMode,
}

#[derive(Debug, Serialize)]
pub struct RotateClaimTokenResponse {
    pub site_id: String,
    pub claim_token: String,
}

#[derive(Debug, Serialize)]
pub struct ConfigSnippetResponse {
    pub site_id: String,
    pub toml: String,
}

#[derive(Debug, Deserialize)]
pub struct UpgradeRoomRequest {
    pub new_version: String,
}

#[derive(Debug, Deserialize)]
pub struct RecoverUpgradeRequest {
    pub new_version: String,
    pub replacement_room: String,
}

#[derive(Debug, Serialize)]
pub struct UpgradeRoomResponse {
    pub room_id: String,
    pub new_version: String,
    pub replacement_room: String,
}

#[derive(Debug, Serialize)]
pub struct RecoverUpgradeResponse {
    pub room_id: String,
    pub new_version: String,
    pub replacement_room: String,
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RetireRoomResponse {
    pub room_id: String,
    pub status: &'static str,
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

pub async fn require_operator(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let presented = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    // A valid operator token bypasses the limiter entirely, so flood traffic
    // can never starve the real operator's access.
    if let (Some(expected), Some(token)) = (&state.operator_token_hash, presented)
        && constant_time_eq(expected.as_bytes(), token_hash(token).as_bytes())
    {
        return next.run(req).await;
    }

    // Missing, wrong or not-enabled requests all share an IP-scoped bucket.
    // Keying by the presented token here would let an attacker mint a fresh
    // quota by simply rotating the token on every attempt.
    let key = client_key(req.headers(), Some(addr), &state.trusted_proxies);
    if !state.operator_limiter.allow(&key) {
        return AppError::TooManyRequests {
            detail: "Operator API is rate limited; try again later".to_string(),
            retry_after_seconds: state.operator_limiter.window().as_secs(),
        }
        .into_response();
    }
    if state.operator_token_hash.is_none() {
        return AppError::Unauthorized(
            "Operator API is not enabled; set `security.operator_token`".to_string(),
        )
        .into_response();
    }
    AppError::Unauthorized("invalid operator token".to_string()).into_response()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_operator_sites_handler(
    method: Method,
    State(state): State<ApiState>,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    if method != *QUERY_METHOD {
        return Err(AppError::MethodNotAllowed);
    }
    let query = parse_operator_list_query(&body)?;

    let page = list_operator_sites(state.store.as_ref(), &state.site_auth_policy, &query)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list sites: {e}")))?;
    Ok((
        [(ACCEPT_QUERY.clone(), "application/json")],
        (StatusCode::OK, Json(page)),
    ))
}

pub(crate) async fn list_quarantined_rooms_handler(
    method: Method,
    State(state): State<ApiState>,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    if method != *QUERY_METHOD {
        return Err(AppError::MethodNotAllowed);
    }
    let query = parse_operator_list_query(&body)?;

    let page = list_operator_quarantined_rooms(state.store.as_ref(), &query)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list quarantined rooms: {e}")))?;
    Ok((
        [(ACCEPT_QUERY.clone(), "application/json")],
        (StatusCode::OK, Json(page)),
    ))
}

/// Clears a room's quarantine and makes it the canonical room again.
///
/// `DELETE` on the quarantine subresource is idempotent: reinstating a room
/// that is already active is a successful no-op. Unknown rooms return 404.
pub(crate) async fn reinstate_room_handler(
    State(state): State<ApiState>,
    Path(room_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let reinstated = state
        .store
        .reinstate_room(&room_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to reinstate room: {e}")))?;
    if reinstated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound("Room not found.".to_string()))
    }
}

pub(crate) async fn upgrade_room_handler(
    State(state): State<ApiState>,
    Path(room_id): Path<String>,
    Json(body): Json<UpgradeRoomRequest>,
) -> Result<impl IntoResponse, AppError> {
    let replacement = cumments_core::management::upgrade_comment_room(
        state.driver.as_ref(),
        state.store.as_ref(),
        &state.site_service,
        &room_id,
        &body.new_version,
    )
    .await
    .map_err(map_management_error)?;
    Ok(Json(UpgradeRoomResponse {
        room_id,
        new_version: body.new_version,
        replacement_room: replacement,
    }))
}

pub(crate) async fn list_upgrade_intents_handler(
    method: Method,
    State(state): State<ApiState>,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    if method != *QUERY_METHOD {
        return Err(AppError::MethodNotAllowed);
    }
    let query: UpgradeIntentListQuery = if body.is_empty() {
        UpgradeIntentListQuery {
            page: None,
            per_page: None,
            status: None,
        }
    } else {
        serde_json::from_str(&body)
            .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))?
    };

    let page = list_operator_upgrade_intents(state.store.as_ref(), &query)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list upgrade intents: {e}")))?;
    Ok((
        [(ACCEPT_QUERY.clone(), "application/json")],
        (StatusCode::OK, Json(page)),
    ))
}

pub(crate) async fn recover_upgrade_intent_handler(
    State(state): State<ApiState>,
    Path(room_id): Path<String>,
    Json(body): Json<RecoverUpgradeRequest>,
) -> Result<impl IntoResponse, AppError> {
    let replacement = cumments_core::management::recover_comment_room_upgrade(
        state.driver.as_ref(),
        state.store.as_ref(),
        &state.site_service,
        &room_id,
        &body.new_version,
        &body.replacement_room,
    )
    .await
    .map_err(map_management_error)?;
    Ok(Json(RecoverUpgradeResponse {
        room_id,
        new_version: body.new_version,
        replacement_room: replacement,
        status: "adopted",
    }))
}

/// Operator mirror for post-level room retirement, keyed by the raw room ID.
pub(crate) async fn retire_room_handler(
    State(state): State<ApiState>,
    Path(room_id): Path<String>,
) -> Result<Json<RetireRoomResponse>, AppError> {
    let retired =
        cumments_core::management::retire_page_room_by_room_id(state.store.as_ref(), &room_id)
            .await
            .map_err(|e| AppError::Internal(format!("failed to retire room: {e}")))?;
    if !retired {
        return Err(AppError::NotFound(
            "room not found or not active".to_string(),
        ));
    }
    state.governance_notify.notify_one();
    Ok(Json(RetireRoomResponse {
        room_id,
        status: "retiring",
    }))
}

/// Parses the optional QUERY body; an empty body means default pagination.
fn parse_operator_list_query(body: &str) -> Result<OperatorListQuery, AppError> {
    if body.is_empty() {
        return Ok(OperatorListQuery {
            page: None,
            per_page: None,
            site_id: None,
        });
    }
    serde_json::from_str(body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))
}

pub(crate) async fn revoke_verified_origin_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    body: String,
) -> Result<Json<OperatorSite>, AppError> {
    let req: RevokeOriginRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let origin = cumments_core::site_auth::Origin::parse(&req.origin)
        .map_err(|e| AppError::BadRequest(format!("invalid origin `{}`: {e}", req.origin)))?;

    if let Some(entry) = state.site_auth_policy.entry(site_id.as_str())
        && entry
            .allowed_origins
            .iter()
            .any(|pattern| pattern.matches(&origin))
    {
        return Err(AppError::BadRequest(
            "origin is declared in the `[sites]` configuration; edit the config file to \
             revoke it"
                .to_string(),
        ));
    }

    let revoked = state
        .store
        .revoke_verified_origin(site_id.as_str(), &origin)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke origin: {e}")))?;
    if !revoked {
        return Err(AppError::NotFound(
            "origin is not verified for this site".to_string(),
        ));
    }

    let info = state
        .store
        .get_site_auth(site_id.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to reload site: {e}")))?
        .ok_or_else(|| AppError::NotFound("site not found".to_string()))?;
    Ok(Json(operator_site(
        &info,
        state.site_auth_policy.entry(site_id.as_str()),
    )))
}

pub(crate) async fn rotate_secret_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
) -> Result<Json<RotateSecretResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    if state
        .site_auth_policy
        .entry(site_id.as_str())
        .is_some_and(|entry| entry.auth_mode == Some(SiteAuthMode::Secret))
    {
        return Err(AppError::BadRequest(
            "site secret is configured in `[sites]`; edit the config file to rotate it".to_string(),
        ));
    }

    if state
        .store
        .get_site_auth(site_id.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to load site: {e}")))?
        .is_none()
    {
        return Err(AppError::NotFound("site not found".to_string()));
    }

    let secret = generate_token();
    state
        .store
        .store_site_secret(site_id.as_str(), &secret)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store secret: {e}")))?;
    Ok(Json(RotateSecretResponse {
        site_id: site_id.as_str().to_string(),
        secret,
    }))
}

pub(crate) async fn revoke_secret_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
) -> Result<Json<RevokeSecretResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    if state
        .site_auth_policy
        .entry(site_id.as_str())
        .is_some_and(|entry| entry.secret.is_some())
    {
        return Err(AppError::BadRequest(
            "site secret is configured in `[sites]`; edit the config file to revoke it".to_string(),
        ));
    }

    let cleared = state
        .store
        .clear_site_secret(site_id.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to clear secret: {e}")))?;
    if !cleared {
        return Err(AppError::NotFound("site not found".to_string()));
    }
    Ok(Json(RevokeSecretResponse {
        site_id: site_id.as_str().to_string(),
        auth_mode: SiteAuthMode::Origin,
    }))
}

pub(crate) async fn rotate_claim_token_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
) -> Result<Json<RotateClaimTokenResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let claim_token =
        cumments_core::management::rotate_claim_token(state.store.as_ref(), site_id.as_str())
            .await
            .map_err(|e| AppError::Internal(format!("failed to rotate claim token: {e}")))?
            .ok_or_else(|| AppError::NotFound("site not found".to_string()))?;
    Ok(Json(RotateClaimTokenResponse {
        site_id: site_id.as_str().to_string(),
        claim_token,
    }))
}

pub(crate) async fn config_snippet_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
) -> Result<Json<ConfigSnippetResponse>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let db_info = state
        .store
        .get_site_auth(site_id.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to load site: {e}")))?;
    let Some(db_info) = db_info else {
        return Err(AppError::NotFound("site not found".to_string()));
    };
    let config_entry = state.site_auth_policy.entry(site_id.as_str());

    Ok(Json(ConfigSnippetResponse {
        site_id: site_id.as_str().to_string(),
        toml: config_snippet_toml(site_id.as_str(), &db_info, config_entry),
    }))
}

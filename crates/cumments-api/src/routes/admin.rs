//! Operator-only admin routes for database-tracked sites.
//!
//! Protected by a static bearer token (`security.admin_token`). Configuration
//! remains the operator's declarative surface: admin endpoints never write
//! config files, they manage runtime state and print adoption snippets.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use crate::request::PaginationMeta;
use crate::routes::comments::{ACCEPT_QUERY, QUERY_METHOD};
use axum::extract::Request;
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{Method, StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use cumments_core::models::SiteId;
use cumments_core::site_auth::{
    SiteAuthInfo, SiteAuthMode, SiteLifecycle, SitePolicyEntry, SiteVerificationStatus,
    constant_time_eq, generate_token, token_hash,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AdminPage<T> {
    pub data: Vec<T>,
    pub meta: PaginationMeta,
}

#[derive(Debug, Serialize)]
pub struct AdminQuarantinedRoom {
    pub room_id: String,
    pub site_id: String,
    pub post_slug: String,
    pub quarantine_reason: String,
    pub quarantined_at: DateTime<Utc>,
    pub adoption_failures: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct AdminSite {
    pub site_id: String,
    pub lifecycle: SiteLifecycle,
    pub auth_mode: SiteAuthMode,
    pub verification_status: SiteVerificationStatus,
    pub origins: Vec<AdminOrigin>,
    pub verified_at: Option<DateTime<Utc>>,
    pub has_secret: bool,
    pub has_claim_token: bool,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct AdminOrigin {
    pub origin: String,
    /// `config` (operator-declared) or `verified` (self-service proof).
    pub source: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct RevokeOriginRequest {
    pub origin: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub site_id: Option<String>,
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

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

pub async fn require_admin(
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
    if let (Some(expected), Some(token)) = (&state.admin_token_hash, presented)
        && constant_time_eq(expected.as_bytes(), token_hash(token).as_bytes())
    {
        return next.run(req).await;
    }

    // Missing, wrong or not-enabled requests all share an IP-scoped bucket.
    // Keying by the presented token here would let an attacker mint a fresh
    // quota by simply rotating the token on every attempt.
    let key = client_key(req.headers(), Some(addr), &state.trusted_proxies);
    if !state.admin_limiter.allow(&key) {
        return AppError::TooManyRequests {
            detail: "admin API is rate limited; try again later".to_string(),
            retry_after_seconds: state.admin_limiter.window().as_secs(),
        }
        .into_response();
    }
    if state.admin_token_hash.is_none() {
        return AppError::Unauthorized(
            "admin API is not enabled; set `security.admin_token`".to_string(),
        )
        .into_response();
    }
    AppError::Unauthorized("invalid admin token".to_string()).into_response()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_admin_sites_handler(
    method: Method,
    State(state): State<ApiState>,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    if method != *QUERY_METHOD {
        return Err(AppError::MethodNotAllowed);
    }
    let query = parse_admin_list_query(&body)?;

    let db_sites = state
        .store
        .list_site_auth()
        .await
        .map_err(|e| AppError::Internal(format!("failed to list sites: {e}")))?;

    let mut sites = db_sites
        .iter()
        .map(|info| admin_site(info, state.site_auth_policy.entry(&info.site_id)))
        .collect::<Vec<_>>();
    let known = sites
        .iter()
        .map(|site| site.site_id.clone())
        .collect::<HashSet<_>>();
    for (site_id, entry) in &state.site_auth_policy.sites {
        if !known.contains(site_id) {
            sites.push(admin_site_from_config(site_id, entry));
        }
    }
    sites.sort_by(|a, b| a.site_id.cmp(&b.site_id));
    if let Some(site_id) = query.site_id.as_deref().filter(|s| !s.is_empty()) {
        sites.retain(|site| site.site_id == site_id);
    }

    let (page, per_page) = admin_page_bounds(&query);
    let total = sites.len() as i64;
    let start = ((page - 1) * per_page) as usize;
    let data = sites
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .collect();
    Ok((
        [(ACCEPT_QUERY.clone(), "application/json")],
        (
            StatusCode::OK,
            Json(AdminPage {
                data,
                meta: admin_meta(total, page, per_page),
            }),
        ),
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
    let query = parse_admin_list_query(&body)?;

    let mut rooms = state
        .store
        .get_quarantined_rooms()
        .await
        .map_err(|e| AppError::Internal(format!("failed to list quarantined rooms: {e}")))?;
    rooms.sort_by(|a, b| a.site_id.cmp(&b.site_id).then(a.room_id.cmp(&b.room_id)));
    if let Some(site_id) = query.site_id.as_deref().filter(|s| !s.is_empty()) {
        rooms.retain(|room| room.site_id == site_id);
    }
    let (page, per_page) = admin_page_bounds(&query);
    let total = rooms.len() as i64;
    let start = ((page - 1) * per_page) as usize;
    let data = rooms
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .map(|room| AdminQuarantinedRoom {
            room_id: room.room_id,
            site_id: room.site_id,
            post_slug: room.post_slug,
            quarantine_reason: room.quarantine_reason,
            quarantined_at: room.quarantined_at,
            adoption_failures: room.adoption_failures,
            next_attempt_at: room.next_attempt_at,
        })
        .collect();
    Ok((
        [(ACCEPT_QUERY.clone(), "application/json")],
        (
            StatusCode::OK,
            Json(AdminPage {
                data,
                meta: admin_meta(total, page, per_page),
            }),
        ),
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

/// Parses the optional QUERY body; an empty body means default pagination.
fn parse_admin_list_query(body: &str) -> Result<AdminListQuery, AppError> {
    if body.is_empty() {
        return Ok(AdminListQuery {
            page: None,
            per_page: None,
            site_id: None,
        });
    }
    serde_json::from_str(body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))
}

pub fn admin_page_bounds(query: &AdminListQuery) -> (i64, i64) {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    (page, per_page)
}

pub fn admin_meta(total: i64, page: i64, per_page: i64) -> PaginationMeta {
    let total_pages = if total > 0 {
        (total + per_page - 1) / per_page
    } else {
        0
    };
    PaginationMeta {
        total,
        page,
        per_page,
        total_pages,
    }
}

pub(crate) async fn revoke_verified_origin_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    body: String,
) -> Result<Json<AdminSite>, AppError> {
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
    Ok(Json(admin_site(
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

/// Builds the TOML block for adopting a database-tracked site into `[sites]`.
///
/// Shared by the admin API and the CLI so both produce identical output.
pub fn config_snippet_toml(
    site_id: &str,
    db_info: &SiteAuthInfo,
    config_entry: Option<&SitePolicyEntry>,
) -> String {
    let mut origins = db_info
        .verified_origins
        .iter()
        .map(|origin| format!("\"{}\"", origin.as_str()))
        .collect::<Vec<_>>();
    if let Some(entry) = config_entry {
        origins.extend(
            entry
                .allowed_origins
                .iter()
                .map(|pattern| format!("\"{}\"", pattern.as_pattern_string())),
        );
    }
    origins.sort();
    origins.dedup();

    let auth_mode = config_entry
        .and_then(|entry| entry.auth_mode)
        .unwrap_or(db_info.auth_mode);
    let mut toml = format!("[sites.\"{}\"]\n", site_id);
    toml.push_str(&format!("auth_mode = \"{}\"\n", auth_mode.as_str()));
    if !origins.is_empty() {
        toml.push_str(&format!("allowed_origins = [{}]\n", origins.join(", ")));
    }
    if auth_mode == SiteAuthMode::Secret {
        toml.push_str(&format!(
            "# Set the secret via environment instead of this file:\n\
             # CUMMENTS__SITES__{}__SECRET=...\n",
            site_id
        ));
    }
    toml
}

// ---------------------------------------------------------------------------
// View helpers
// ---------------------------------------------------------------------------

pub fn admin_site(info: &SiteAuthInfo, config: Option<&SitePolicyEntry>) -> AdminSite {
    let mut origins = info
        .verified_origins
        .iter()
        .map(|origin| AdminOrigin {
            origin: origin.as_str().to_string(),
            source: "verified",
        })
        .collect::<Vec<_>>();
    if let Some(entry) = config {
        origins.extend(entry.allowed_origins.iter().map(|pattern| AdminOrigin {
            origin: pattern.as_pattern_string(),
            source: "config",
        }));
    }
    origins.sort_by(|a, b| a.origin.cmp(&b.origin));
    origins.dedup_by(|a, b| a.origin == b.origin);

    AdminSite {
        site_id: info.site_id.clone(),
        lifecycle: info.lifecycle,
        auth_mode: config
            .and_then(|entry| entry.auth_mode)
            .unwrap_or(info.auth_mode),
        verification_status: if config.is_some() {
            SiteVerificationStatus::Verified
        } else {
            info.verification_status
        },
        origins,
        verified_at: info.verified_at,
        has_secret: config.is_some_and(|entry| entry.secret.is_some()) || info.secret.is_some(),
        has_claim_token: info.claim_token_hash.is_some(),
        updated_at: info.updated_at,
    }
}

pub fn admin_site_from_config(site_id: &str, entry: &SitePolicyEntry) -> AdminSite {
    AdminSite {
        site_id: site_id.to_string(),
        lifecycle: SiteLifecycle::Active,
        auth_mode: entry.auth_mode.unwrap_or(SiteAuthMode::Origin),
        verification_status: SiteVerificationStatus::Verified,
        origins: entry
            .allowed_origins
            .iter()
            .map(|pattern| AdminOrigin {
                origin: pattern.as_pattern_string(),
                source: "config",
            })
            .collect(),
        verified_at: None,
        has_secret: entry.secret.is_some(),
        has_claim_token: false,
        updated_at: None,
    }
}

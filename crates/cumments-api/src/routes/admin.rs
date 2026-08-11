//! Operator-only admin routes for database-tracked sites.
//!
//! Protected by a static bearer token (`security.admin_token`). Configuration
//! remains the operator's declarative surface: admin endpoints never write
//! config files, they manage runtime state and print adoption snippets.

use crate::ApiState;
use crate::error::AppError;
use axum::extract::Request;
use axum::{
    Json,
    extract::{Path, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use cumments_core::models::SiteId;
use cumments_core::site_auth::{
    SiteAuthInfo, SiteAuthMode, SitePolicyEntry, SiteVerificationStatus, constant_time_eq,
    generate_token, token_hash,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AdminSiteList {
    pub sites: Vec<AdminSite>,
}

#[derive(Debug, Serialize)]
pub struct AdminSite {
    pub site_id: String,
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
pub struct ConfigSnippetResponse {
    pub site_id: String,
    pub toml: String,
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

pub async fn require_admin(State(state): State<ApiState>, req: Request, next: Next) -> Response {
    let Some(expected) = &state.admin_token_hash else {
        return AppError::Unauthorized(
            "admin API is not enabled; set `security.admin_token`".to_string(),
        )
        .into_response();
    };
    let presented = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match presented {
        Some(token) if constant_time_eq(expected.as_bytes(), token_hash(token).as_bytes()) => {
            next.run(req).await
        }
        _ => AppError::Unauthorized("invalid admin token".to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_admin_sites_handler(
    State(state): State<ApiState>,
) -> Result<Json<AdminSiteList>, AppError> {
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

    Ok(Json(AdminSiteList { sites }))
}

pub(crate) async fn revoke_verified_origin_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    Json(req): Json<RevokeOriginRequest>,
) -> Result<Json<AdminSite>, AppError> {
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
    let mut toml = format!("[sites.\"{}\"]\n", site_id.as_str());
    toml.push_str(&format!("auth_mode = \"{}\"\n", auth_mode.as_str()));
    if !origins.is_empty() {
        toml.push_str(&format!("allowed_origins = [{}]\n", origins.join(", ")));
    }
    if auth_mode == SiteAuthMode::Secret {
        toml.push_str(&format!(
            "# Set the secret via environment instead of this file:\n\
             # CUMMENTS__SITES__{}__SECRET=...\n",
            site_id.as_str()
        ));
    }

    Ok(Json(ConfigSnippetResponse {
        site_id: site_id.as_str().to_string(),
        toml,
    }))
}

// ---------------------------------------------------------------------------
// View helpers
// ---------------------------------------------------------------------------

fn admin_site(info: &SiteAuthInfo, config: Option<&SitePolicyEntry>) -> AdminSite {
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

fn admin_site_from_config(site_id: &str, entry: &SitePolicyEntry) -> AdminSite {
    AdminSite {
        site_id: site_id.to_string(),
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

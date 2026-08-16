//! Public self-service guest profile reads.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use crate::routes::media::media_url_base;
use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use cumments_core::identity::{derive_guest_id_from_public_key, parse_public_key};
use cumments_core::models::SiteId;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;

/// `GET /api/v1/sites/{site_id}/guests/profile?author_public_key=...`
///
/// Public self-service read of the guest's current global profile (display
/// name and avatar) for this site. The virtual user is derived from
/// `site_id + public_key`, and the avatar is rewritten to a signed proxy
/// URL like comment authors.
pub(crate) async fn guest_profile_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.guest_profile_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "guest profile lookups are rate limited; try again later".to_string(),
            retry_after_seconds: state.guest_profile_limiter.window().as_secs(),
        });
    }

    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    // A missing parent site is a 404; a missing guest profile on an existing
    // site is still a 200 empty profile.
    if state
        .store
        .get_site(&site_id_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up site: {e}")))?
        .is_none()
    {
        return Err(AppError::NotFound("Site not found.".to_string()));
    }
    let author_public_key = query
        .get("author_public_key")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("missing author_public_key".to_string()))?;
    if parse_public_key(&author_public_key).is_none() {
        return Err(AppError::BadRequest(
            "author_public_key must be a valid base64url Ed25519 public key".to_string(),
        ));
    }
    let guest_id =
        derive_guest_id_from_public_key(&author_public_key).expect("public key already validated");

    let profile = state
        .driver
        .get_profile(&author_public_key, &site_id_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read guest profile: {e}")))?;

    let media_base = media_url_base(&state, &headers, Some(addr));
    let avatar_url = profile
        .as_ref()
        .and_then(|profile| profile.avatar_url.clone())
        .map(|url| {
            state
                .media_proxy
                .as_ref()
                .and_then(|proxy| proxy.proxify_avatar(&url, &media_base))
                .unwrap_or(url)
        });

    Ok(Json(json!({
        "guest_id": guest_id,
        "display_name": profile.as_ref().and_then(|p| p.display_name.clone()),
        "avatar_url": avatar_url,
    })))
}

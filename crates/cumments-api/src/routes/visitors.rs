//! Public self-service visitor profile reads and authenticated profile mutations.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use crate::request::{
    ClearAvatarRequest, ClearDisplayNameRequest, IDEMPOTENT_REPLAYED, ProfileOperationResponse,
    SetAvatarRequest, SetDisplayNameRequest, extract_idempotency_key,
};
use crate::routes::media::media_url_base;
use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use cumments_core::identity::{derive_visitor_id_from_public_key, parse_public_key};
use cumments_core::media_reference::MediaReference;
use cumments_core::models::SiteId;
use cumments_core::profile::{
    ProfileClaimOutcome, ProfileOperationExecutor, ProfileOperationStatus, ProfileTargetValue,
    verify_profile_signature,
};
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use validator::Validate;

/// `GET /api/v1/sites/{site_id}/visitors/profile?author_public_key=...`
///
/// Public self-service read of the visitor's current global profile (display
/// name and avatar) for this site. The virtual user is derived from
/// `site_id + public_key`. Authoritative runtime state is read from the
/// Matrix global profile. If an avatar is present and durably mapped,
/// its `MediaReference` is returned. Never exposes raw Matrix MXC URIs in
/// public JSON. Performs no database write transactions.
pub(crate) async fn visitor_profile_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.visitor_profile_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "visitor profile lookups are rate limited; try again later".to_string(),
            retry_after_seconds: state.visitor_profile_limiter.window().as_secs(),
        });
    }

    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    // A missing parent site is a 404; a missing visitor profile on an existing
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
    let visitor_id = derive_visitor_id_from_public_key(&author_public_key)
        .expect("public key already validated");

    let profile = state
        .driver
        .get_profile(&author_public_key, &site_id_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read visitor profile: {e}")))?;

    let (avatar, avatar_url) = match profile.as_ref().and_then(|p| p.avatar_url.as_ref()) {
        Some(mxc) if mxc.starts_with("mxc://") => {
            let record = state
                .store
                .find_reference(&site_id_val, mxc)
                .await
                .map_err(|e| {
                    AppError::Internal(format!("failed to lookup media reference: {e}"))
                })?;
            let media_ref = record.map(|r| r.to_string());
            let media_base = media_url_base(&state, &headers, Some(addr));
            let presentation_url = state
                .media_proxy
                .as_ref()
                .and_then(|proxy| proxy.proxify_avatar(mxc, &media_base));
            (media_ref, presentation_url)
        }
        _ => (None, None),
    };

    Ok(Json(json!({
        "visitor_id": visitor_id,
        "display_name": profile.as_ref().and_then(|p| p.display_name.clone()),
        "avatar": avatar,
        "avatar_url": avatar_url,
    })))
}

/// `PUT /api/v1/sites/{site_id}/visitors/profile/display_name`
pub(crate) async fn set_visitor_display_name_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
    body: String,
) -> Result<Response, AppError> {
    let req: SetDisplayNameRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))?;
    req.validate().map_err(AppError::Validation)?;
    let target = ProfileTargetValue::SetDisplayName(req.display_name);
    process_profile_mutation(
        &state,
        &headers,
        Some(addr),
        &site_id,
        &req.author_public_key,
        &req.author_signature,
        &req.challenge_response,
        target,
    )
    .await
}

/// `DELETE /api/v1/sites/{site_id}/visitors/profile/display_name`
pub(crate) async fn clear_visitor_display_name_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
    body: String,
) -> Result<Response, AppError> {
    let req: ClearDisplayNameRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))?;
    req.validate().map_err(AppError::Validation)?;
    let target = ProfileTargetValue::ClearDisplayName;
    process_profile_mutation(
        &state,
        &headers,
        Some(addr),
        &site_id,
        &req.author_public_key,
        &req.author_signature,
        &req.challenge_response,
        target,
    )
    .await
}

/// `PUT /api/v1/sites/{site_id}/visitors/profile/avatar`
pub(crate) async fn set_visitor_avatar_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
    body: String,
) -> Result<Response, AppError> {
    let req: SetAvatarRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))?;
    req.validate().map_err(AppError::Validation)?;
    let media_ref = MediaReference::parse(&req.avatar)
        .map_err(|e| AppError::BadRequest(format!("invalid media reference: {e}")))?;
    let target = ProfileTargetValue::SetAvatar(media_ref);
    process_profile_mutation(
        &state,
        &headers,
        Some(addr),
        &site_id,
        &req.author_public_key,
        &req.author_signature,
        &req.challenge_response,
        target,
    )
    .await
}

/// `DELETE /api/v1/sites/{site_id}/visitors/profile/avatar`
pub(crate) async fn clear_visitor_avatar_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
    body: String,
) -> Result<Response, AppError> {
    let req: ClearAvatarRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))?;
    req.validate().map_err(AppError::Validation)?;
    let target = ProfileTargetValue::ClearAvatar;
    process_profile_mutation(
        &state,
        &headers,
        Some(addr),
        &site_id,
        &req.author_public_key,
        &req.author_signature,
        &req.challenge_response,
        target,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn process_profile_mutation(
    state: &ApiState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
    site_id_str: &str,
    author_public_key: &str,
    author_signature: &str,
    challenge_response: &str,
    target_value: ProfileTargetValue,
) -> Result<Response, AppError> {
    let key = client_key(headers, addr, &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "write requests are rate limited; try again later".to_string(),
            retry_after_seconds: state.write_limiter.window().as_secs(),
        });
    }

    let idempotency_key = extract_idempotency_key(headers)?;
    let site_id = SiteId::new(site_id_str.to_string()).map_err(AppError::Validation)?;

    if state
        .store
        .get_site(&site_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up site: {e}")))?
        .is_none()
    {
        return Err(AppError::NotFound("Site not found.".to_string()));
    }

    if parse_public_key(author_public_key).is_none() {
        return Err(AppError::BadRequest(
            "author_public_key must be a valid base64url Ed25519 public key".to_string(),
        ));
    }

    // Avatar MediaReference validation: must resolve through Stage C store for this site
    if let ProfileTargetValue::SetAvatar(ref media_ref) = target_value {
        let resolved = state
            .store
            .resolve_mxc(&site_id, media_ref)
            .await
            .map_err(|e| AppError::Internal(format!("failed to resolve media reference: {e}")))?;
        if resolved.is_none() {
            return Err(AppError::NotFound(format!(
                "Media reference '{media_ref}' not found for site '{}'",
                site_id.as_str()
            )));
        }
    }

    // Verify Ed25519 signature over canonical semantic profile mutation envelope
    if !verify_profile_signature(
        author_public_key,
        &target_value,
        site_id.as_str(),
        &idempotency_key,
        author_signature,
    ) {
        return Err(AppError::InvalidSignature);
    }

    // Independently verify PoW freshness at HTTP admission boundary
    if !state.pow.verify(challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // Claim or get operation in durable ProfileStore
    let claim_res = state
        .store
        .claim_or_get_profile_operation(
            &idempotency_key,
            author_public_key,
            &site_id,
            &target_value,
        )
        .await
        .map_err(|e| AppError::Internal(format!("failed to claim profile operation: {e}")))?;

    let (op, replayed) = match claim_res {
        ProfileClaimOutcome::New(op) => {
            state.submission_notify.notify_one();
            (op, false)
        }
        ProfileClaimOutcome::Replay(op) => (op, true),
        ProfileClaimOutcome::Conflict => return Err(AppError::IdempotencyReused),
    };

    let executor = ProfileOperationExecutor::new(
        state.store.clone(),
        state.driver.clone(),
        Some(state.store.clone()),
    );

    // If new or pending on replay, attempt execution
    if !replayed || op.status == ProfileOperationStatus::Pending {
        let _ = executor
            .execute(&idempotency_key)
            .await
            .map_err(|e| AppError::Internal(format!("failed to execute profile operation: {e}")))?;
    }

    // Read updated status from store
    let op = state
        .store
        .get_profile_operation(&idempotency_key)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read profile operation: {e}")))?
        .unwrap_or(op);

    if op.status == ProfileOperationStatus::Failed || op.status == ProfileOperationStatus::Aborted {
        return Err(AppError::BadRequest(op.error_detail.unwrap_or_else(|| {
            "profile operation failed downstream".to_string()
        })));
    }

    let status_code = match op.status {
        ProfileOperationStatus::Completed => StatusCode::OK,
        ProfileOperationStatus::Pending
        | ProfileOperationStatus::Dispatching
        | ProfileOperationStatus::Unknown => StatusCode::ACCEPTED,
        ProfileOperationStatus::Failed | ProfileOperationStatus::Aborted => unreachable!(),
    };

    let resp_body = ProfileOperationResponse {
        operation_id: op.operation_id,
        status: op.status,
        field: op.field,
        value: op.target_value.to_stored(),
        error: op.error_detail,
    };

    let mut response = (status_code, Json(resp_body)).into_response();
    if replayed {
        response.headers_mut().insert(
            IDEMPOTENT_REPLAYED.clone(),
            HeaderValue::from_static("true"),
        );
    }

    Ok(response)
}

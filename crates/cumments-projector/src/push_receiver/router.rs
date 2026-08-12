//! Axum router and HTTP handlers for the AppService push endpoint.

use super::auth::{hs_token_matches, received_hs_token};
use super::parsers::process_single_event;
use super::state::{ProcessedTxnSet, PushState};
use super::types::Transaction;
use crate::event_processor::EventProcessor;
use axum::{
    Json,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{post, put},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

// ── Axum router ──────────────────────────────────────────────────

/// Build the axum router for the AppService push endpoint.
///
/// # Panics
/// The `hs_token` is read from the standard `Authorization: Bearer` header
/// (with the legacy `?hs_token=` query parameter as a fallback) and compared
/// against the configured value. Requests without a valid token are rejected
/// with 403 FORBIDDEN, matching the AppService API's `M_FORBIDDEN` error.
pub fn push_router(processor: Arc<EventProcessor>, hs_token: String) -> axum::Router {
    let state = Arc::new(PushState {
        processor,
        hs_token,
        processed_txns: Mutex::new(ProcessedTxnSet::new()),
    });

    axum::Router::new()
        .route(
            "/_matrix/app/v1/transactions/{txnId}",
            put(handle_transaction),
        )
        .route("/transactions/{txnId}", put(handle_transaction))
        .route("/_matrix/app/v1/ping", post(handle_ping))
        .with_state(state)
}

/// Like [`push_router`] but with an `M_UNRECOGNIZED` fallback for unknown
/// routes.
///
/// Use this only for standalone (dedicated-port) deployments where the router
/// owns every unmatched path. When the push routes are merged into the API
/// router, axum allows only one fallback per merged router, so the shared-port
/// build keeps the API router's behaviour.
pub fn push_router_standalone(processor: Arc<EventProcessor>, hs_token: String) -> axum::Router {
    push_router(processor, hs_token).fallback(handle_unknown)
}

/// Respond to unknown AppService routes with the spec's `M_UNRECOGNIZED`
/// error instead of an empty 404.
async fn handle_unknown() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "errcode": "M_UNRECOGNIZED",
            "error": "Unrecognized request"
        })),
    )
}

/// Handle `POST /_matrix/app/v1/ping` (AppService API v1.7+).
///
/// The homeserver uses this to verify reachability and `hs_token` correctness
/// when an appservice calls `POST /_matrix/client/v1/appservice/{id}/ping`.
async fn handle_ping(
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    state: axum::extract::State<Arc<PushState>>,
    _body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    if !hs_token_matches(&headers, &query, &state.hs_token) {
        tracing::warn!("AppService ping rejected: invalid hs_token");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"errcode": "M_FORBIDDEN", "error": "Invalid hs_token"})),
        );
    }
    (StatusCode::OK, Json(serde_json::json!({})))
}

/// Handle `PUT /_matrix/app/v1/transactions/{txnId}`.
async fn handle_transaction(
    Path(txn_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    state: axum::extract::State<Arc<PushState>>,
    Json(txn): Json<Transaction>,
) -> impl IntoResponse {
    if txn_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"errcode": "M_INVALID_PARAM", "error": "txnId required"})),
        );
    }
    if state
        .processed_txns
        .lock()
        .expect("processed txns mutex poisoned")
        .contains(&txn_id)
    {
        return (StatusCode::OK, Json(serde_json::json!({})));
    }

    // ── hs_token verification ──
    let received = received_hs_token(&headers, &query);
    if received != Some(state.hs_token.as_str()) {
        tracing::warn!(
            "Push transaction rejected: invalid hs_token (received: {:?})",
            received.map(|s| &s[..8.min(s.len())])
        );
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"errcode": "M_FORBIDDEN", "error": "Invalid hs_token"})),
        );
    }

    let mut failed = false;
    for event in txn.events {
        if let Err(e) = process_single_event(&event, &state.processor).await {
            tracing::warn!("Failed to process push event: {:?}", e);
            failed = true;
        }
    }

    if failed {
        // Ask the homeserver to retry the whole transaction instead of
        // acknowledging events that were never projected.
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "errcode": "M_UNKNOWN",
                "error": "Failed to process push transaction"
            })),
        );
    }

    let mut processed = state
        .processed_txns
        .lock()
        .expect("processed txns mutex poisoned");
    processed.insert(txn_id);

    // The AppService protocol requires an empty JSON object response.
    (StatusCode::OK, Json(serde_json::json!({})))
}

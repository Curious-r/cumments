//! Axum router and HTTP handlers for the AppService push endpoint.

use super::auth::hs_token_matches;
use super::parsers::process_single_event;
use super::state::PushState;
use super::types::Transaction;
use crate::event_processor::EventProcessor;
use axum::{
    Json,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{post, put},
};
use cumments_core::ports::{AppServiceTxnStore, SseOutboxStore};
use std::{collections::HashMap, sync::Arc};

// ── Axum router ──────────────────────────────────────────────────

/// Build the axum router for the AppService push endpoint.
///
/// # Panics
/// The `hs_token` is read from the standard `Authorization: Bearer` header
/// (with the legacy `?hs_token=` query parameter as a fallback) and compared
/// against the configured value. Requests without a valid token are rejected
/// with 403 FORBIDDEN, matching the AppService API's `M_FORBIDDEN` error.
pub fn push_router(
    processor: Arc<EventProcessor>,
    txn_store: Arc<dyn AppServiceTxnStore>,
    outbox_store: Arc<dyn SseOutboxStore>,
    hs_token: String,
) -> axum::Router {
    let state = Arc::new(PushState {
        processor,
        hs_token,
        txn_store,
        outbox_store,
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
pub fn push_router_standalone(
    processor: Arc<EventProcessor>,
    txn_store: Arc<dyn AppServiceTxnStore>,
    outbox_store: Arc<dyn SseOutboxStore>,
    hs_token: String,
) -> axum::Router {
    push_router(processor, txn_store, outbox_store, hs_token).fallback(handle_unknown)
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

fn internal_error(message: &'static str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"errcode": "M_UNKNOWN", "error": message})),
    )
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

    // ── hs_token verification (constant time) ──
    // Authentication happens before the txn-id replay short-circuit so
    // unauthenticated callers cannot probe which transaction IDs have been
    // acknowledged.
    if !hs_token_matches(&headers, &query, &state.hs_token) {
        tracing::warn!("Push transaction rejected: invalid hs_token");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"errcode": "M_FORBIDDEN", "error": "Invalid hs_token"})),
        );
    }

    if let Ok(true) = state.txn_store.has_processed_txn(&txn_id).await {
        return (StatusCode::OK, Json(serde_json::json!({})));
    }

    let mut failed = false;
    let sse_event_id_prefix = format!("sse:{txn_id}");
    for (event_index, event) in txn.events.into_iter().enumerate() {
        let sse_event_id = format!("{sse_event_id_prefix}:{event_index}");
        let should_process = match state
            .outbox_store
            .reserve_sse_outbox(&txn_id, event_index as u32, &sse_event_id)
            .await
        {
            Ok(should_process) => should_process,
            Err(error) => {
                tracing::error!("Failed to reserve SSE outbox: {error:#}");
                return internal_error("Failed to reserve projection output");
            }
        };
        if !should_process {
            continue;
        }

        state.processor.start_event_capture().await;
        let result = process_single_event(&event, &state.processor).await;
        let captured = state.processor.stop_event_capture().await;
        if let Some(events) = captured
            && let Ok(payload) = serde_json::to_string(&events)
            && let Err(error) = state
                .outbox_store
                .fill_sse_outbox(&sse_event_id, &payload)
                .await
        {
            tracing::error!("Failed to persist SSE output: {error:#}");
            failed = true;
            continue;
        }
        if let Err(e) = result {
            tracing::warn!("Failed to process push event: {:?}", e);
            failed = true;
        }
    }

    if failed {
        // Ask the homeserver to retry the whole transaction instead of
        // acknowledging events that were never projected.
        return internal_error("Failed to process push transaction");
    }

    if let Err(error) = state.txn_store.mark_processed_txn(&txn_id).await {
        tracing::error!("Failed to record processed push transaction: {error:#}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "errcode": "M_UNKNOWN",
                "error": "Failed to record transaction"
            })),
        );
    }

    // The AppService protocol requires an empty JSON object response.
    (StatusCode::OK, Json(serde_json::json!({})))
}

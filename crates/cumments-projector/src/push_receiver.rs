//! PushReceiver – AppService push event endpoint.
//!
//! Receives events pushed by the Matrix homeserver via
//! `PUT /_matrix/app/v1/transactions/{txnId}?hs_token={hs_token}`
//! and feeds them into the transport-agnostic [`EventProcessor`].
//! The `hs_token` query parameter is verified against the configured
//! value before any events are processed.

use crate::event_processor::{
    EventProcessor, ParsedRelation, ParsedRoomMessage, ParsedRoomRedaction, ParsedSpaceChild,
};
use axum::{
    Json,
    extract::{Path, Query},
    http::StatusCode,
    response::IntoResponse,
    routing::put,
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

// ── Shared state ──────────────────────────────────────────────────

/// Shared state for the push receiver endpoints.
pub struct PushState {
    processor: Arc<EventProcessor>,
    hs_token: String,
}

// ── Matrix AppService push event types ────────────────────────────

/// The top-level push transaction payload.
#[derive(Deserialize)]
struct Transaction {
    events: Vec<PushEvent>,
}

/// A single event from the AppService push transaction.
#[derive(Deserialize)]
pub(crate) struct PushEvent {
    #[serde(rename = "type")]
    event_type: String,
    event_id: Option<String>,
    room_id: Option<String>,
    sender: Option<String>,
    origin_server_ts: Option<i64>,
    state_key: Option<String>,
    content: Option<serde_json::Value>,
    /// The event this event redacts (for redaction events).
    redacts: Option<String>,
    /// Whether the event has been redacted.
    #[allow(dead_code)]
    unsigned: Option<UnsignedData>,
}

#[derive(Deserialize)]
struct UnsignedData {
    // Ignored for now – may contain redacted_because etc.
}

// ── Axum router ──────────────────────────────────────────────────

/// Build the axum router for the AppService push endpoint.
///
/// # Panics
/// The `hs_token` is compared against the `hs_token` query parameter
/// sent by the homeserver. Requests without a valid token are rejected
/// with 401 UNAUTHORIZED.
pub fn push_router(processor: Arc<EventProcessor>, hs_token: String) -> axum::Router {
    let state = Arc::new(PushState {
        processor,
        hs_token,
    });

    axum::Router::new()
        .route(
            "/_matrix/app/v1/transactions/{txnId}",
            put(handle_transaction),
        )
        .with_state(state)
}

/// Handle `PUT /_matrix/app/v1/transactions/{txnId}`.
async fn handle_transaction(
    Path(_txn_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    state: axum::extract::State<Arc<PushState>>,
    Json(txn): Json<Transaction>,
) -> impl IntoResponse {
    // ── hs_token verification ──
    let received = query.get("hs_token").map(|s| s.as_str());
    if received != Some(state.hs_token.as_str()) {
        tracing::warn!(
            "Push transaction rejected: invalid hs_token (received: {:?})",
            received.map(|s| &s[..8.min(s.len())])
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"errcode": "M_FORBIDDEN", "error": "Invalid hs_token"})),
        );
    }

    for event in txn.events {
        if let Err(e) = process_single_event(&event, &state.processor).await {
            tracing::warn!("Failed to process push event: {:?}", e);
        }
    }

    // The AppService protocol requires an empty JSON object response.
    (StatusCode::OK, Json(serde_json::json!({})))
}

// ── Event dispatch ────────────────────────────────────────────────

/// Route a single push event to the appropriate processor method.
pub(crate) async fn process_single_event(
    event: &PushEvent,
    processor: &EventProcessor,
) -> anyhow::Result<()> {
    let event_type = event.event_type.as_str();

    match event_type {
        "m.room.message" => {
            if let Some(mut parsed) = parse_push_message(event) {
                parsed.room_identity = processor.resolve_room_identity(&parsed.room_id).await;
                processor.process_room_message(parsed).await;
            }
        }
        "m.room.redaction" => {
            if let Some(mut parsed) = parse_push_redaction(event) {
                parsed.room_identity = processor.resolve_room_identity(&parsed.room_id).await;
                processor.process_room_redaction(parsed).await;
            }
        }
        "m.space.child" => {
            // Resolve the site_id from the space's room_id in local DB
            if let Some(ref space_room_id) = event.room_id {
                let site_id = processor.get_site_id_by_space_id(space_room_id).await;
                if let Some(mut parsed) = parse_push_space_child(event, site_id).await {
                    parsed.child_room_identity =
                        processor.resolve_room_identity(&parsed.child_room_id).await;
                    processor.process_space_child(parsed).await;
                }
            }
        }
        _ => {
            // Ignore other event types
        }
    }

    Ok(())
}

// ── Push event helpers ────────────────────────────────────────────

/// Extract the display name from a Cumments message body.
///
/// The AppServiceMatrixDriver formats the body as:
///   `**nickname**: comment content`
///
/// In push mode we don't have access to the room member list, so we
/// parse the nickname back out of the body.
fn extract_author_display_name(body: &str) -> Option<String> {
    body.strip_prefix("**")
        .and_then(|s| s.split_once("**: "))
        .map(|(nick, _)| nick.to_string())
        .filter(|n| !n.is_empty())
}

/// Structured `cumments_content` takes precedence. Otherwise, for
/// Cumments-generated (legacy) messages, strip the `**nickname**: ` prefix
/// from the body; external Matrix messages are treated as plain content.
fn extract_message_content(body: &str, is_cumments: bool, structured: Option<&str>) -> String {
    if let Some(c) = structured {
        return c.to_string();
    }
    if is_cumments
        && let Some((_, rest)) = body.strip_prefix("**").and_then(|s| s.split_once("**: "))
    {
        return rest.to_string();
    }
    body.to_string()
}

// ── Push event parsers ────────────────────────────────────────────

/// Parse a push message event into a `ParsedRoomMessage`.
fn parse_push_message(event: &PushEvent) -> Option<ParsedRoomMessage> {
    let room_id = event.room_id.as_ref()?;
    let event_id = event.event_id.as_ref()?;
    let sender = event.sender.as_ref()?;
    let content = event.content.as_ref()?;

    // Extract message body
    let body = content.get("body").and_then(|v| v.as_str())?;

    // Extract the public identity fields. The visitor id's presence marks this
    // as a Cumments-generated message for legacy body parsing.
    let visitor_id = content
        .get("cumments_visitor_id")
        .or_else(|| content.get("cumments_public_key"))
        .and_then(|v| v.as_str());
    let author_public_key = content
        .get("cumments_public_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let author_signature = content
        .get("cumments_signature")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let structured_content = content.get("cumments_content").and_then(|v| v.as_str());
    let structured_nickname = content.get("cumments_nickname").and_then(|v| v.as_str());

    // Extract relation (edit)
    let relates_to = content.get("m.relates_to").and_then(|rel| {
        let rel_type = rel.get("rel_type").and_then(|v| v.as_str())?;
        if rel_type != "m.replace" {
            return None;
        }
        let target_event_id = rel.get("event_id").and_then(|v| v.as_str())?;
        let new_content = rel.get("m.new_content").and_then(|nc| {
            let nc_body = nc.get("body").and_then(|v| v.as_str())?;
            let nc_structured = nc.get("cumments_content").and_then(|v| v.as_str());
            let nc_is_cumments = nc.get("cumments_visitor_id").is_some();
            Some(extract_message_content(
                nc_body,
                nc_is_cumments,
                nc_structured,
            ))
        })?;
        Some(ParsedRelation {
            target_event_id: target_event_id.to_string(),
            new_content: new_content.to_string(),
        })
    });

    // Room identity is resolved by the caller from the local registry:
    // push events carry only the event content, not room state metadata.
    let room_identity = None;

    let origin_server_ts = event.origin_server_ts.unwrap_or(0);

    Some(ParsedRoomMessage {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        sender: sender.clone(),
        content: extract_message_content(body, visitor_id.is_some(), structured_content),
        author_display_name: structured_nickname
            .map(|s| s.to_string())
            .or_else(|| extract_author_display_name(body)),
        author_public_key,
        author_signature,
        intent_id: content.get("cumments_intent_id").and_then(|v| v.as_i64()),
        origin_server_ts,
        relates_to,
        room_identity,
    })
}

/// Parse a push redaction event into a `ParsedRoomRedaction`.
fn parse_push_redaction(event: &PushEvent) -> Option<ParsedRoomRedaction> {
    let room_id = event.room_id.as_ref()?;
    let event_id = event.event_id.as_ref()?;

    let redacts = event.redacts.as_ref().map(|s| s.to_string()).or_else(|| {
        event
            .content
            .as_ref()
            .and_then(|c| c.get("redacts").and_then(|v| v.as_str().map(String::from)))
    });

    // Room identity is resolved by the caller from the local registry.
    let room_identity = None;

    Some(ParsedRoomRedaction {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        redacts,
        room_identity,
    })
}

/// Parse a push space child event into a `ParsedSpaceChild`.
/// `site_id` is resolved from the local database before calling this.
async fn parse_push_space_child(
    event: &PushEvent,
    site_id: Option<String>,
) -> Option<ParsedSpaceChild> {
    let room_id = event.room_id.as_ref()?;
    let state_key = event.state_key.as_ref()?;
    let content = event.content.as_ref()?;

    let space_room_id = room_id.clone();
    let child_room_id = state_key.clone();

    // Determine if attached (via list non-empty) or removed.
    let is_attached = content
        .get("via")
        .and_then(|v| v.as_array())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);

    // Child room identity requires an additional HS API call;
    // left as None for now (Future improvement).
    let child_room_identity = None;

    Some(ParsedSpaceChild {
        space_room_id,
        site_id,
        child_room_id,
        is_attached,
        child_room_identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_content_takes_precedence() {
        assert_eq!(
            extract_message_content("**Alice**: old body", true, Some("pure markdown content")),
            "pure markdown content"
        );
    }

    #[test]
    fn legacy_cumments_body_strips_nickname_prefix() {
        assert_eq!(
            extract_message_content("**Alice**: hello world", true, None),
            "hello world"
        );
    }

    #[test]
    fn external_message_body_is_plain_content() {
        assert_eq!(
            extract_message_content("just a comment", false, None),
            "just a comment"
        );
        // An external message that happens to start with bold markup must not
        // be treated as a Cumments legacy body.
        assert_eq!(
            extract_message_content("**bold** start", false, None),
            "**bold** start"
        );
    }

    #[test]
    fn author_display_name_from_structured_or_legacy_body() {
        assert_eq!(
            extract_author_display_name("**Alice**: hi"),
            Some("Alice".into())
        );
        assert_eq!(extract_author_display_name("plain body"), None);
    }
}

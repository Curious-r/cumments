//! PushReceiver – AppService push event endpoint.
//!
//! Receives events pushed by the Matrix homeserver via
//! `PUT /_matrix/app/v1/transactions/{txnId}` and feeds them into
//! the transport-agnostic [`EventProcessor`].

use crate::event_processor::{
    EventProcessor, ParsedRelation, ParsedRoomMessage, ParsedRoomRedaction, ParsedSpaceChild,
    parse_room_identity,
};
use axum::{Json, extract::Path, http::StatusCode, response::IntoResponse, routing::put};
use serde::Deserialize;
use std::sync::Arc;

// ── Matrix AppService push event types ────────────────────────────

/// The top-level push transaction payload.
#[derive(Deserialize)]
struct Transaction {
    events: Vec<PushEvent>,
}

/// A single event from the AppService push transaction.
#[derive(Deserialize)]
struct PushEvent {
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
pub fn push_router(processor: Arc<EventProcessor>) -> axum::Router {
    axum::Router::new()
        .route(
            "/_matrix/app/v1/transactions/{txnId}",
            put(handle_transaction),
        )
        .with_state(processor)
}

/// Handle `PUT /_matrix/app/v1/transactions/{txnId}`.
async fn handle_transaction(
    Path(_txn_id): Path<String>,
    processor: axum::extract::State<Arc<EventProcessor>>,
    Json(txn): Json<Transaction>,
) -> impl IntoResponse {
    for event in txn.events {
        if let Err(e) = process_single_event(&event, &processor).await {
            tracing::warn!("Failed to process push event: {:?}", e);
        }
    }

    // The AppService protocol requires an empty JSON object response.
    (StatusCode::OK, Json(serde_json::json!({})))
}

// ── Event dispatch ────────────────────────────────────────────────

/// Route a single push event to the appropriate processor method.
async fn process_single_event(
    event: &PushEvent,
    processor: &EventProcessor,
) -> Result<(), Box<dyn std::error::Error>> {
    let event_type = event.event_type.as_str();

    match event_type {
        "m.room.message" => {
            if let Some(parsed) = parse_push_message(event) {
                processor.process_room_message(parsed).await;
            }
        }
        "m.room.redaction" => {
            if let Some(parsed) = parse_push_redaction(event) {
                processor.process_room_redaction(parsed).await;
            }
        }
        "m.space.child" => {
            if let Some(parsed) = parse_push_space_child(event, processor).await {
                processor.process_space_child(parsed).await;
            }
        }
        _ => {
            // Ignore other event types
        }
    }

    Ok(())
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

    // Extract fingerprint
    let fingerprint = content
        .get("cumments_author_fingerprint")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Extract relation (edit)
    let relates_to = content.get("m.relates_to").and_then(|rel| {
        let rel_type = rel.get("rel_type").and_then(|v| v.as_str())?;
        if rel_type != "m.replace" {
            return None;
        }
        let target_event_id = rel.get("event_id").and_then(|v| v.as_str())?;
        let new_content = rel
            .get("m.new_content")
            .and_then(|nc| nc.get("body").and_then(|v| v.as_str()))?;
        Some(ParsedRelation {
            target_event_id: target_event_id.to_string(),
            new_content: new_content.to_string(),
        })
    });

    // Check if it's an edit (skip original content if edit)
    if relates_to.is_some() {
        // For edits, use the body from m.new_content
        // but the processor will use relates_to.new_content
        // so body here is just a placeholder
    }

    // Resolve room identity from metadata if available
    let metadata_json = content.get("im.cumments.metadata").and_then(|v| v.as_str());
    let room_identity = parse_room_identity(metadata_json, None);

    let origin_server_ts = event.origin_server_ts.unwrap_or(0);

    Some(ParsedRoomMessage {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        sender: sender.clone(),
        content: body.to_string(),
        author_display_name: None, // Push events don't include member info
        fingerprint,
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

    // Resolve room identity
    let metadata_json = event
        .content
        .as_ref()
        .and_then(|c| c.get("im.cumments.metadata").and_then(|v| v.as_str()));
    let room_identity = parse_room_identity(metadata_json, None);

    Some(ParsedRoomRedaction {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        redacts,
        room_identity,
    })
}

/// Parse a push space child event into a `ParsedSpaceChild`.
// The processor parameter is reserved for future use (resolving child room identity).
#[allow(dead_code)]
async fn parse_push_space_child(
    event: &PushEvent,
    _processor: &EventProcessor,
) -> Option<ParsedSpaceChild> {
    let room_id = event.room_id.as_ref()?;
    let state_key = event.state_key.as_ref()?;
    let content = event.content.as_ref()?;

    // The room_id in the push event is the Space's ID.
    let space_room_id = room_id.clone();
    let child_room_id = state_key.clone();

    // Determine if attached (via list non-empty) or removed.
    let is_attached = content
        .get("via")
        .and_then(|v| v.as_array())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);

    // Attempt to resolve site_id from the Space's metadata.
    // In push mode, the event itself doesn't carry the Space's
    // metadata. We need to either:
    //   (a) Query the HS API for the Space's state, or
    //   (b) Look up the space in our local registry.
    // For now, we rely on the local database or leave it None.
    let site_id = None; // Future: query HS via appservice API

    // For the child room identity, push events don't include
    // room state. We'd need to make an additional API call.
    let child_room_identity = None; // Future: query HS via appservice API

    Some(ParsedSpaceChild {
        space_room_id,
        site_id,
        child_room_id,
        is_attached,
        child_room_identity,
    })
}

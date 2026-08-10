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
use cumments_core::protocol::MESSAGE_CONTENT_KEY;
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

/// Structured Cumments content takes precedence. Otherwise, for
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

/// Read a string field from the Cumments content block, falling back to the
/// block inside the standard `m.new_content` replacement payload.
fn content_string<'a>(content: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    content
        .get(MESSAGE_CONTENT_KEY)
        .and_then(|ns| ns.get(key))
        .or_else(|| {
            content
                .get("m.new_content")
                .and_then(|nc| nc.get(MESSAGE_CONTENT_KEY))
                .and_then(|ns| ns.get(key))
        })
        .and_then(|v| v.as_str())
}

/// Read an integer field from the same Cumments content block locations.
fn content_i64(content: &serde_json::Value, key: &str) -> Option<i64> {
    content
        .get(MESSAGE_CONTENT_KEY)
        .and_then(|ns| ns.get(key))
        .or_else(|| {
            content
                .get("m.new_content")
                .and_then(|nc| nc.get(MESSAGE_CONTENT_KEY))
                .and_then(|ns| ns.get(key))
        })
        .and_then(|v| v.as_i64())
}

/// Whether a Matrix sender is one of our exclusive AS virtual users.
///
/// The AppService registration reserves the `@_cumments_.*` user namespace,
/// so a localpart starting with `_cumments_` can only belong to Cumments.
fn is_virtual_user_sender(sender: &str) -> bool {
    sender
        .strip_prefix('@')
        .and_then(|s| s.split_once(':'))
        .map(|(localpart, _)| localpart.starts_with("_cumments_"))
        .unwrap_or(false)
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

    let is_virtual_sender = is_virtual_user_sender(sender);

    // Structured Cumments fields are only trusted for our virtual users.
    // Matrix-native senders may copy a block into their event; it must be
    // ignored so it cannot be used to impersonate a guest identity.
    let author_public_key = content_string(content, "public_key").map(|s| s.to_string());
    let author_signature = content_string(content, "signature").map(|s| s.to_string());
    let author_challenge = content_string(content, "challenge").map(|s| s.to_string());
    let structured_content = content_string(content, "content");
    let structured_nickname = content_string(content, "nickname");
    let visitor_id =
        content_string(content, "visitor_id").or_else(|| content_string(content, "public_key"));

    let trusted_block = is_virtual_sender
        && author_public_key.is_some()
        && author_signature.is_some()
        && author_challenge.is_some()
        && structured_content.is_some()
        && structured_nickname.is_some();

    // Extract the standard rich-reply relation, if any.
    let reply_to = content
        .get("m.relates_to")
        .and_then(|rel| rel.get("m.in_reply_to"))
        .and_then(|reply| reply.get("event_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Extract relation (edit)
    let relates_to = content.get("m.relates_to").and_then(|rel| {
        let rel_type = rel.get("rel_type").and_then(|v| v.as_str())?;
        if rel_type != "m.replace" {
            return None;
        }
        let target_event_id = rel.get("event_id").and_then(|v| v.as_str())?;
        // `m.new_content` is a top-level content property, per the Matrix spec;
        // it must not be read from inside `m.relates_to`.
        let new_content = content.get("m.new_content").and_then(|nc| {
            let nc_body = nc.get("body").and_then(|v| v.as_str())?;
            let nc_namespace = nc.get(MESSAGE_CONTENT_KEY);
            let nc_structured = nc_namespace
                .and_then(|ns| ns.get("content"))
                .and_then(|v| v.as_str());
            let nc_is_cumments = nc_namespace.is_some();
            Some(extract_message_content(
                nc_body,
                is_virtual_sender && nc_is_cumments,
                if is_virtual_sender {
                    nc_structured
                } else {
                    None
                },
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
        content: extract_message_content(
            body,
            is_virtual_sender && visitor_id.is_some(),
            if is_virtual_sender {
                structured_content
            } else {
                None
            },
        ),
        author_display_name: if is_virtual_sender {
            structured_nickname.map(|s| s.to_string())
        } else {
            None
        },
        author_public_key: if trusted_block {
            author_public_key
        } else {
            None
        },
        author_signature: if trusted_block {
            author_signature
        } else {
            None
        },
        author_challenge: if trusted_block {
            author_challenge
        } else {
            None
        },
        is_virtual_sender,
        intent_id: if trusted_block {
            content_i64(content, "intent_id")
        } else {
            None
        },
        reply_to,
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
    fn edit_event_carries_intent_id_for_precise_closed_loop() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$edit:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_abcd:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "body": " * **Alice**: edited",
                "m.new_content": {
                    "body": "**Alice**: edited",
                    "host.curious.cumments": {
                        "visitor_id": "abcd",
                        "public_key": "pubkey",
                        "signature": "sig",
                        "challenge": "chal",
                        "content": "edited",
                        "nickname": "Alice",
                        "intent_id": 42,
                    }
                },
                "m.relates_to": {
                    "rel_type": "m.replace",
                    "event_id": "$original:hs",
                },
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_message(&event).expect("parse edit");
        assert_eq!(parsed.author_display_name.as_deref(), Some("Alice"));
        assert_eq!(parsed.author_public_key.as_deref(), Some("pubkey"));
        assert_eq!(parsed.author_signature.as_deref(), Some("sig"));
        assert_eq!(parsed.content, "edited");
        assert_eq!(parsed.intent_id, Some(42));
        assert_eq!(
            parsed
                .relates_to
                .as_ref()
                .map(|r| r.target_event_id.as_str()),
            Some("$original:hs")
        );
        assert_eq!(
            parsed.relates_to.as_ref().map(|r| r.new_content.as_str()),
            Some("edited")
        );
    }

    #[test]
    fn new_message_reads_namespaced_content_block() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$comment:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_abcd:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "**Alice**: hello",
                "host.curious.cumments": {
                    "visitor_id": "abcd",
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                    "nickname": "Alice",
                    "intent_id": 7,
                }
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_message(&event).expect("parse comment");
        assert_eq!(parsed.author_display_name.as_deref(), Some("Alice"));
        assert_eq!(parsed.author_public_key.as_deref(), Some("pubkey"));
        assert_eq!(parsed.author_signature.as_deref(), Some("sig"));
        assert_eq!(parsed.content, "hello");
        assert_eq!(parsed.intent_id, Some(7));
        assert!(parsed.relates_to.is_none());
    }

    #[test]
    fn reply_event_parses_standard_relation() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$reply:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_abcd:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "**Alice**: hello",
                "m.relates_to": {
                    "m.in_reply_to": {
                        "event_id": "$parent:hs",
                    }
                },
                "host.curious.cumments": {
                    "visitor_id": "abcd",
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                    "nickname": "Alice",
                    "intent_id": 7,
                }
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_message(&event).expect("parse reply");
        assert_eq!(parsed.reply_to.as_deref(), Some("$parent:hs"));
        assert!(parsed.relates_to.is_none());
    }

    #[test]
    fn matrix_native_message_ignores_cumments_block() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$native:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@alice:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "plain body",
                "host.curious.cumments": {
                    "visitor_id": "abcd",
                    "public_key": "fake-pubkey",
                    "signature": "fake-signature",
                    "challenge": "fake-challenge",
                    "content": "spoofed content",
                    "nickname": "Spoofed",
                    "intent_id": 42,
                }
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_message(&event).expect("parse native message");
        assert!(!parsed.is_virtual_sender);
        assert_eq!(parsed.content, "plain body");
        assert!(parsed.author_public_key.is_none());
        assert!(parsed.author_signature.is_none());
        assert!(parsed.author_challenge.is_none());
        assert!(parsed.author_display_name.is_none());
        assert!(parsed.intent_id.is_none());
    }

    #[test]
    fn virtual_user_message_without_full_block_is_untrusted() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$guest:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_abcd:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "legacy body",
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_message(&event).expect("parse guest message");
        assert!(parsed.is_virtual_sender);
        assert!(parsed.author_public_key.is_none());
        assert!(parsed.author_signature.is_none());
        assert!(parsed.author_challenge.is_none());
        assert!(parsed.intent_id.is_none());
    }
}

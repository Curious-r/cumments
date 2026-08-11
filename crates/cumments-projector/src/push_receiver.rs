//! PushReceiver – AppService push event endpoint.
//!
//! Receives events pushed by the Matrix homeserver via
//! `PUT /_matrix/app/v1/transactions/{txnId}`.
//! and feeds them into the transport-agnostic [`EventProcessor`].
//! The `hs_token` is verified against the configured value before any events
//! are processed. Per the Matrix AppService specification the token arrives
//! in the `Authorization: Bearer` header; the legacy `?hs_token=` query
//! parameter is also accepted for homeservers that use it. When both are
//! present they must agree, per the spec's compatibility guidance.

use crate::event_processor::{
    EventProcessor, ParsedRelation, ParsedRoomMessage, ParsedRoomRedaction, ParsedSpaceChild,
};
use axum::{
    Json,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{post, put},
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
/// The `hs_token` is read from the standard `Authorization: Bearer` header
/// (with the legacy `?hs_token=` query parameter as a fallback) and compared
/// against the configured value. Requests without a valid token are rejected
/// with 403 FORBIDDEN, matching the AppService API's `M_FORBIDDEN` error.
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
    Json(_body): Json<serde_json::Value>,
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
    Path(_txn_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    state: axum::extract::State<Arc<PushState>>,
    Json(txn): Json<Transaction>,
) -> impl IntoResponse {
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

    // The AppService protocol requires an empty JSON object response.
    (StatusCode::OK, Json(serde_json::json!({})))
}

/// Read the `hs_token` from the `Authorization: Bearer` header.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty())
}

/// Resolve the `hs_token` from the standard header and/or the legacy query
/// parameter. The header takes precedence; when both are supplied they must
/// match, otherwise no token is considered valid.
fn received_hs_token<'a>(
    headers: &'a HeaderMap,
    query: &'a HashMap<String, String>,
) -> Option<&'a str> {
    let header_token = bearer_token(headers);
    let query_token = query.get("hs_token").map(|s| s.as_str());
    match (header_token, query_token) {
        (Some(header), Some(query)) if header == query => Some(header),
        (Some(header), None) => Some(header),
        (None, Some(query)) => Some(query),
        _ => None,
    }
}

/// Whether the resolved `hs_token` matches the configured value.
fn hs_token_matches(headers: &HeaderMap, query: &HashMap<String, String>, expected: &str) -> bool {
    received_hs_token(headers, query) == Some(expected)
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
                parsed.room_identity = processor.resolve_room_identity(&parsed.room_id).await?;
                processor.process_room_message(parsed).await?;
            }
        }
        "m.room.redaction" => {
            if let Some(mut parsed) = parse_push_redaction(event) {
                parsed.room_identity = processor.resolve_room_identity(&parsed.room_id).await?;
                processor.process_room_redaction(parsed).await?;
            }
        }
        "m.space.child" => {
            // Resolve the site_id from the space's room_id in local DB
            if let Some(ref space_room_id) = event.room_id {
                let site_id = processor.get_site_id_by_space_id(space_room_id).await?;
                if let Some(mut parsed) = parse_push_space_child(event, site_id).await {
                    parsed.child_room_identity = processor
                        .resolve_room_identity(&parsed.child_room_id)
                        .await?;
                    processor.process_space_child(parsed).await?;
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

/// Structured Cumments content takes precedence over the message body.
/// External Matrix messages (no structured block) use the plain body.
fn extract_message_content(body: &str, structured: Option<&str>) -> String {
    if let Some(c) = structured {
        return c.to_string();
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
/// Virtual user localparts follow `_cumments_{site_id}_{visitor_id}`, where
/// the site is `[a-z0-9-]{1,64}` and the visitor is 16 lowercase hex digits.
/// Matching the exact shape (rather than the broader `@_cumments_.*`
/// namespace) excludes the AS sender account itself (`@_cumments_bot`) and
/// any other Cumments-reserved lookalikes from being treated as visitors.
fn is_virtual_user_sender(sender: &str) -> bool {
    let Some(localpart) = sender
        .strip_prefix('@')
        .and_then(|s| s.split_once(':').map(|(localpart, _)| localpart))
    else {
        return false;
    };
    let Some(rest) = localpart.strip_prefix("_cumments_") else {
        return false;
    };
    let Some((site_id, visitor_id)) = rest.rsplit_once('_') else {
        return false;
    };
    let site_ok = !site_id.is_empty()
        && site_id.len() <= 64
        && site_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let visitor_ok = visitor_id.len() == 16
        && visitor_id
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    site_ok && visitor_ok
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
    let structured_displayname = content_string(content, "displayname");

    let trusted_block = is_virtual_sender
        && author_public_key.is_some()
        && author_signature.is_some()
        && author_challenge.is_some()
        && structured_content.is_some()
        && structured_displayname.is_some();

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
            Some(extract_message_content(
                nc_body,
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
            if is_virtual_sender {
                structured_content
            } else {
                None
            },
        ),
        author_display_name: if is_virtual_sender {
            structured_displayname.map(|s| s.to_string())
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

    // Cumments deletes embed a signed JSON proof in the redaction's `reason`.
    let proof = event
        .content
        .as_ref()
        .and_then(|c| c.get("reason"))
        .and_then(|v| v.as_str())
        .and_then(|reason| serde_json::from_str(reason).ok());

    // Room identity is resolved by the caller from the local registry.
    let room_identity = None;

    Some(ParsedRoomRedaction {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        redacts,
        proof,
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
    use axum::http::HeaderValue;

    #[test]
    fn bearer_token_accepts_standard_header() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer abc123"));
        assert_eq!(bearer_token(&headers), Some("abc123"));
    }

    #[test]
    fn bearer_token_scheme_is_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("bearer abc123"));
        assert_eq!(bearer_token(&headers), Some("abc123"));
    }

    #[test]
    fn bearer_token_rejects_missing_or_empty() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer"));
        assert_eq!(bearer_token(&headers), None);

        headers.insert("authorization", HeaderValue::from_static("Basic abc123"));
        assert_eq!(bearer_token(&headers), None);

        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    fn headers_with_bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_str(token).unwrap());
        headers
    }

    #[test]
    fn hs_token_accepts_header_only() {
        let query = HashMap::new();
        assert_eq!(
            received_hs_token(&headers_with_bearer("Bearer abc123"), &query),
            Some("abc123")
        );
    }

    #[test]
    fn hs_token_accepts_legacy_query_only() {
        let headers = HeaderMap::new();
        let mut query = HashMap::new();
        query.insert("hs_token".to_string(), "abc123".to_string());
        assert_eq!(received_hs_token(&headers, &query), Some("abc123"));
    }

    #[test]
    fn hs_token_requires_agreement_when_both_are_present() {
        let mut query = HashMap::new();
        query.insert("hs_token".to_string(), "query123".to_string());

        // Both present and matching: accepted.
        assert_eq!(
            received_hs_token(&headers_with_bearer("Bearer query123"), &query),
            Some("query123")
        );
        // Both present but disagreeing: no token is valid.
        assert_eq!(
            received_hs_token(&headers_with_bearer("Bearer header123"), &query),
            None
        );
    }

    #[test]
    fn hs_token_rejects_missing_tokens() {
        assert_eq!(received_hs_token(&HeaderMap::new(), &HashMap::new()), None);
    }

    #[test]
    fn virtual_user_sender_matches_exact_namespace_shape() {
        assert!(is_virtual_user_sender(
            "@_cumments_my-blog_3282f2a21b4a1e6b:example.com"
        ));
        assert!(is_virtual_user_sender(
            "@_cumments_a_3282f2a21b4a1e6b:example.com"
        ));
        assert!(is_virtual_user_sender(
            "@_cumments_my-blog_3282f2a21b4a1e6b:other.com"
        ));
    }

    #[test]
    fn virtual_user_sender_rejects_as_sender_and_lookalikes() {
        // The AS sender account lives inside the exclusive namespace but is
        // not a visitor virtual user.
        assert!(!is_virtual_user_sender("@_cumments_bot:example.com"));
        // Wrong visitor length or alphabet.
        assert!(!is_virtual_user_sender(
            "@_cumments_my-blog_abcd:example.com"
        ));
        assert!(!is_virtual_user_sender(
            "@_cumments_my-blog_3282F2A21B4A1E6B:example.com"
        ));
        // Site ids cannot contain underscores.
        assert!(!is_virtual_user_sender(
            "@_cumments_my_blog_3282f2a21b4a1e6b:example.com"
        ));
        // Non-Cumments senders.
        assert!(!is_virtual_user_sender("@alice:example.com"));
        assert!(!is_virtual_user_sender(
            "_cumments_my-blog_3282f2a21b4a1e6b"
        ));
    }

    #[test]
    fn redaction_parse_extracts_embedded_delete_proof() {
        let event = PushEvent {
            event_type: "m.room.redaction".to_string(),
            event_id: Some("$redaction:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_bot:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "reason": "{\"host.curious.cumments.redaction\":{\"site_id\":\"my-blog\",\"target_event_id\":\"$target:hs\"}}",
                "redacts": "$target:hs",
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_redaction(&event).expect("parse redaction");
        assert_eq!(parsed.redacts.as_deref(), Some("$target:hs"));
        let proof = parsed.proof.expect("proof parsed from reason");
        assert_eq!(
            proof["host.curious.cumments.redaction"]["site_id"].as_str(),
            Some("my-blog")
        );
    }

    #[test]
    fn redaction_parse_without_proof_yields_none() {
        let event = PushEvent {
            event_type: "m.room.redaction".to_string(),
            event_id: Some("$redaction:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@alice:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({ "reason": "manual moderation" })),
            redacts: None,
            unsigned: None,
        };
        let parsed = parse_push_redaction(&event).expect("parse redaction");
        assert!(parsed.proof.is_none());
    }

    #[test]
    fn structured_content_takes_precedence() {
        assert_eq!(
            extract_message_content("**Alice**: old body", Some("pure markdown content")),
            "pure markdown content"
        );
    }

    #[test]
    fn external_message_body_is_plain_content() {
        assert_eq!(
            extract_message_content("just a comment", None),
            "just a comment"
        );
        assert_eq!(
            extract_message_content("**bold** start", None),
            "**bold** start"
        );
    }

    #[test]
    fn edit_event_carries_intent_id_for_precise_closed_loop() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$edit:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "body": " * **Alice**: edited",
                "m.new_content": {
                    "body": "**Alice**: edited",
                    "host.curious.cumments.message": {
                        "visitor_id": "abcd",
                        "public_key": "pubkey",
                        "signature": "sig",
                        "challenge": "chal",
                        "content": "edited",
                        "displayname": "Alice",
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
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "**Alice**: hello",
                "host.curious.cumments.message": {
                    "visitor_id": "abcd",
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                    "displayname": "Alice",
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
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b:hs".to_string()),
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
                "host.curious.cumments.message": {
                    "visitor_id": "abcd",
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                    "displayname": "Alice",
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
                "host.curious.cumments.message": {
                    "visitor_id": "abcd",
                    "public_key": "fake-pubkey",
                    "signature": "fake-signature",
                    "challenge": "fake-challenge",
                    "content": "spoofed content",
                    "displayname": "Spoofed",
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
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b:hs".to_string()),
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

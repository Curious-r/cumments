//! Parsing and dispatch of Matrix push events into processor inputs.

use super::types::PushEvent;
use crate::event_processor::EventProcessor;
use crate::parsed::{
    ParsedPollVote, ParsedReaction, ParsedRelation, ParsedRoomMessage, ParsedRoomRedaction,
    ParsedRoomState, ParsedSpaceChild,
};
use cumments_core::models::{
    Content, EncryptedPlaceholder, LocationContent, MediaContent, MediaKind, PollContent,
    PollOption, TextContent, TextStyle, UnknownContent,
};
use cumments_core::protocol::{MESSAGE_CONTENT_KEY, REDACTION_PROOF_KEY};

// ── Event dispatch ────────────────────────────────────────────────

/// Route a single push event to the appropriate processor method.
pub(crate) async fn process_single_event(
    event: &PushEvent,
    processor: &EventProcessor,
) -> anyhow::Result<()> {
    let event_type = event.event_type.as_str();

    match event_type {
        "m.room.message" | "m.sticker" | "m.room.encrypted" => {
            if let Some(vote) = parse_push_poll_vote(event) {
                processor.process_poll_vote(vote).await?;
                return Ok(());
            }
            if let Some(mut parsed) = parse_push_message(event) {
                if processor.process_bot_command(&parsed).await? {
                    return Ok(());
                }
                if processor.process_claim_dm(&parsed).await? {
                    return Ok(());
                }
                parsed.room_identity = processor.resolve_room_identity(&parsed.room_id).await?;
                processor.process_room_message(parsed).await?;
            }
        }
        "m.reaction" => {
            if let Some(mut parsed) = parse_push_reaction(event) {
                parsed.room_identity = processor.resolve_room_identity(&parsed.room_id).await?;
                processor.process_reaction(parsed).await?;
            }
        }
        "m.room.member"
        | "m.room.name"
        | "m.room.topic"
        | "m.room.avatar"
        | "m.room.canonical_alias"
        | "m.room.power_levels"
        | "m.room.tombstone"
        | "m.room.join_rules"
        | "m.room.history_visibility"
        | "m.room.guest_access"
        | "m.room.encryption"
        | "m.room.pinned_events"
        | "m.room.create" => {
            if let Some(parsed) = parse_push_state(event) {
                processor.process_room_state(parsed).await?;
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

/// Read a string field from the Cumments content block, falling back to the
/// block inside the standard `m.new_content` replacement payload.
fn namespaced_string<'a>(content: &'a serde_json::Value, key: &str) -> Option<&'a str> {
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
fn namespaced_i64(content: &serde_json::Value, key: &str) -> Option<i64> {
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
/// Virtual user localparts follow `_cumments_{site_id}_{guest_id}`, where
/// the site is `[a-z0-9-]{1,64}` and the visitor is 32 lowercase hex digits.
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
    let Some((site_id, guest_id)) = rest.rsplit_once('_') else {
        return false;
    };
    let site_ok = !site_id.is_empty()
        && site_id.len() <= 64
        && site_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let guest_ok =
        guest_id.len() == 32 && guest_id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    site_ok && guest_ok
}

// ── Push event parsers ────────────────────────────────────────────

/// Parse a push message event into a `ParsedRoomMessage`.
fn parse_push_message(event: &PushEvent) -> Option<ParsedRoomMessage> {
    let room_id = event.room_id.as_ref()?;
    let event_id = event.event_id.as_ref()?;
    let sender = event.sender.as_ref()?;
    let content = event.content.as_ref()?;

    let is_virtual_sender = is_virtual_user_sender(sender);

    let msgtype = if event.event_type == "m.sticker" {
        Some("m.sticker")
    } else {
        content.get("msgtype").and_then(|v| v.as_str())
    };
    // Poll responses are routed to the poll store, not projected as messages.
    if msgtype == Some("org.matrix.msc3381.poll.response") {
        return None;
    }

    // Structured Cumments fields are only trusted for our virtual users.
    // Matrix-native senders may copy a block into their event; it must be
    // ignored so it cannot be used to impersonate a guest identity.
    let author_public_key = namespaced_string(content, "public_key").map(|s| s.to_string());
    let author_signature = namespaced_string(content, "signature").map(|s| s.to_string());
    let author_challenge = namespaced_string(content, "challenge").map(|s| s.to_string());
    let structured_content = namespaced_string(content, "content");
    let structured_display_name = namespaced_string(content, "displayname");

    let trusted_block = is_virtual_sender
        && author_public_key.is_some()
        && author_signature.is_some()
        && author_challenge.is_some()
        && structured_content.is_some()
        && structured_display_name.is_some();

    let mut parsed_content = if event.event_type == "m.room.encrypted" {
        Content::Encrypted(EncryptedPlaceholder {
            algorithm: content
                .get("algorithm")
                .and_then(|v| v.as_str())
                .unwrap_or("m.unknown")
                .to_string(),
            sender_key: content
                .get("sender_key")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        })
    } else {
        parse_message_content(content, msgtype)
    };
    // For guest messages, the structured block takes precedence over the
    // plain-text body (only text messages are signable today).
    if is_virtual_sender
        && let Content::Text(text) = &mut parsed_content
        && let Some(structured) = structured_content
    {
        text.body = structured.to_string();
    }

    // Extract the standard rich-reply relation, if any.
    let reply_to = content
        .get("m.relates_to")
        .and_then(|rel| rel.get("m.in_reply_to"))
        .and_then(|reply| reply.get("event_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Extract the thread relation (m.thread), if any.
    let thread_root = content.get("m.relates_to").and_then(|rel| {
        let rel_type = rel.get("rel_type").and_then(|v| v.as_str())?;
        if rel_type != "m.thread" {
            return None;
        }
        rel.get("event_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    // Extract relation (edit)
    let relates_to = content.get("m.relates_to").and_then(|rel| {
        let rel_type = rel.get("rel_type").and_then(|v| v.as_str())?;
        if rel_type != "m.replace" {
            return None;
        }
        let target_event_id = rel.get("event_id").and_then(|v| v.as_str())?;
        // `m.new_content` is a top-level content property, per the Matrix spec;
        // it must not be read from inside `m.relates_to`.
        let new_content = content.get("m.new_content").map(|nc| {
            let nc_msgtype = nc.get("msgtype").and_then(|v| v.as_str()).or(msgtype);
            let mut parsed = parse_message_content(nc, nc_msgtype);
            // For guest edits, the structured block inside m.new_content still
            // takes precedence over the plain-text body.
            if is_virtual_sender
                && let Content::Text(text) = &mut parsed
                && let Some(structured) = nc
                    .get(MESSAGE_CONTENT_KEY)
                    .and_then(|ns| ns.get("content"))
                    .and_then(|v| v.as_str())
            {
                text.body = structured.to_string();
            }
            parsed
        })?;
        Some(ParsedRelation {
            target_event_id: target_event_id.to_string(),
            new_content,
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
        content: parsed_content,
        display_name: if is_virtual_sender {
            structured_display_name.map(|s| s.to_string())
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
        is_virtual_user_sender: is_virtual_sender,
        submission_id: if trusted_block {
            namespaced_i64(content, "submission_id")
        } else {
            None
        },
        reply_to,
        thread_root,
        origin_server_ts,
        relates_to,
        room_identity,
        raw_content: content.clone(),
    })
}

/// Build the typed `Content` for a message-content object, dispatching on the
/// Matrix `msgtype` (or the event type for stickers/encrypted).
fn parse_message_content(content: &serde_json::Value, msgtype: Option<&str>) -> Content {
    let body = content.get("body").and_then(|v| v.as_str()).unwrap_or("");
    match msgtype {
        Some("m.text") | Some("m.notice") | Some("m.emote") | None => Content::Text(TextContent {
            body: body.to_string(),
            formatted_body: content
                .get("formatted_body")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            style: match msgtype {
                Some("m.notice") => TextStyle::Notice,
                Some("m.emote") => TextStyle::Emote,
                _ => TextStyle::Normal,
            },
        }),
        Some("m.image") => media_content(MediaKind::Image, content, body),
        Some("m.video") => media_content(MediaKind::Video, content, body),
        Some("m.audio") => media_content(MediaKind::Audio, content, body),
        Some("m.file") => media_content(MediaKind::File, content, body),
        Some("m.sticker") => media_content(MediaKind::Sticker, content, body),
        Some("org.matrix.msc3488.location") => location_content(content, body),
        Some("org.matrix.msc3381.poll.start") => poll_content(content, body),
        _ => Content::Unknown(UnknownContent {
            fallback: (!body.is_empty()).then(|| body.to_string()),
            raw: content.clone(),
        }),
    }
}

fn media_content(kind: MediaKind, content: &serde_json::Value, body: &str) -> Content {
    let Some(url) = content.get("url").and_then(|v| v.as_str()) else {
        return Content::Unknown(UnknownContent {
            fallback: Some(body.to_string()),
            raw: content.clone(),
        });
    };
    let info = content.get("info");
    let dimension = |key: &str| {
        info.and_then(|i| i.get(key))
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
    };
    Content::Media(MediaContent {
        kind,
        url: url.to_string(),
        filename: content
            .get("filename")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| (!body.is_empty()).then(|| body.to_string())),
        mimetype: info
            .and_then(|i| i.get("mimetype"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        size: info.and_then(|i| i.get("size")).and_then(|v| v.as_u64()),
        width: dimension("width"),
        height: dimension("height"),
        thumbnail_url: content
            .get("thumbnail_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        alt_text: content
            .get("alt_text")
            .or_else(|| content.get("org.matrix.msc3245.alt_text"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        voice: content
            .get("org.matrix.msc3245.voice")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

fn location_content(content: &serde_json::Value, body: &str) -> Content {
    let Some(geo_uri) = content.get("geo_uri").and_then(|v| v.as_str()) else {
        return Content::Unknown(UnknownContent {
            fallback: Some(body.to_string()),
            raw: content.clone(),
        });
    };
    Content::Location(LocationContent {
        geo_uri: geo_uri.to_string(),
        description: (!body.is_empty()).then(|| body.to_string()),
        thumbnail_url: content
            .get("thumbnail_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

fn poll_content(content: &serde_json::Value, body: &str) -> Content {
    let Some(poll) = content.get("org.matrix.msc3381.poll.start") else {
        return Content::Unknown(UnknownContent {
            fallback: Some(body.to_string()),
            raw: content.clone(),
        });
    };
    let question = poll
        .get("question")
        .and_then(|q| q.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let options = poll
        .get("answers")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|answer| {
            let id = answer.get("id").and_then(|v| v.as_str())?;
            let text = answer
                .get("org.matrix.msc3381.poll.answer")
                .and_then(|a| a.get("text"))
                .and_then(|v| v.as_str())?;
            Some(PollOption {
                id: id.to_string(),
                text: text.to_string(),
            })
        })
        .collect();
    Content::Poll(PollContent {
        question,
        options,
        responses: Vec::new(),
    })
}

/// Parse a reaction event (`m.reaction`) into a `ParsedReaction`.
fn parse_push_reaction(event: &PushEvent) -> Option<ParsedReaction> {
    let room_id = event.room_id.as_ref()?;
    let event_id = event.event_id.as_ref()?;
    let sender = event.sender.as_ref()?;
    let content = event.content.as_ref()?;
    let relates_to = content.get("m.relates_to")?;
    if relates_to.get("rel_type").and_then(|v| v.as_str()) != Some("m.annotation") {
        return None;
    }
    let message_event_id = relates_to.get("event_id").and_then(|v| v.as_str())?;
    let key = relates_to.get("key").and_then(|v| v.as_str())?;
    let is_virtual_user_sender = is_virtual_user_sender(sender);
    Some(ParsedReaction {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        sender: sender.clone(),
        message_event_id: message_event_id.to_string(),
        key: key.to_string(),
        origin_server_ts: event.origin_server_ts.unwrap_or(0),
        is_virtual_user_sender,
        author_public_key: namespaced_string(content, "public_key").map(|s| s.to_string()),
        author_signature: namespaced_string(content, "signature").map(|s| s.to_string()),
        author_challenge: namespaced_string(content, "challenge").map(|s| s.to_string()),
        room_identity: None,
    })
}

/// Parse a poll response (`m.room.message` with
/// `msgtype: org.matrix.msc3381.poll.response`) into a `ParsedPollVote`.
fn parse_push_poll_vote(event: &PushEvent) -> Option<ParsedPollVote> {
    let room_id = event.room_id.as_ref()?;
    let event_id = event.event_id.as_ref()?;
    let sender = event.sender.as_ref()?;
    let content = event.content.as_ref()?;
    if content.get("msgtype").and_then(|v| v.as_str()) != Some("org.matrix.msc3381.poll.response") {
        return None;
    }
    let response = content.get("org.matrix.msc3381.poll.response")?;
    let answer_ids = response
        .get("answers")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    let poll_message_id = content
        .get("m.relates_to")
        .and_then(|rel| rel.get("event_id"))
        .and_then(|v| v.as_str())?
        .to_string();
    let is_virtual_user_sender = is_virtual_user_sender(sender);
    Some(ParsedPollVote {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        sender: sender.clone(),
        poll_message_id,
        answer_ids,
        origin_server_ts: event.origin_server_ts.unwrap_or(0),
        is_virtual_user_sender,
        author_public_key: namespaced_string(content, "public_key").map(|s| s.to_string()),
        author_signature: namespaced_string(content, "signature").map(|s| s.to_string()),
        author_challenge: namespaced_string(content, "challenge").map(|s| s.to_string()),
        room_identity: None,
    })
}

/// Parse a room state event (system message / room metadata).
fn parse_push_state(event: &PushEvent) -> Option<ParsedRoomState> {
    let room_id = event.room_id.as_ref()?;
    let event_id = event.event_id.as_ref()?;
    let sender = event.sender.as_ref()?;
    Some(ParsedRoomState {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        sender: sender.clone(),
        event_type: event.event_type.clone(),
        state_key: event.state_key.clone().unwrap_or_default(),
        origin_server_ts: event.origin_server_ts.unwrap_or(0),
        content: event.content.clone().unwrap_or(serde_json::Value::Null),
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
    let proof: Option<serde_json::Value> = event
        .content
        .as_ref()
        .and_then(|c| c.get("reason"))
        .and_then(|v| v.as_str())
        .and_then(|reason| serde_json::from_str(reason).ok());
    let submission_id = proof
        .as_ref()
        .and_then(|proof: &serde_json::Value| proof.get(REDACTION_PROOF_KEY))
        .and_then(|block| block.get("submission_id"))
        .and_then(|value| value.as_i64());

    // Room identity is resolved by the caller from the local registry.
    let room_identity = None;

    Some(ParsedRoomRedaction {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        sender: event.sender.clone(),
        origin_server_ts: event.origin_server_ts.unwrap_or(0),
        redacts,
        proof,
        submission_id,
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
    fn virtual_user_sender_matches_exact_namespace_shape() {
        assert!(is_virtual_user_sender(
            "@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:example.com"
        ));
        assert!(is_virtual_user_sender(
            "@_cumments_a_3282f2a21b4a1e6b3282f2a21b4a1e6b:example.com"
        ));
        assert!(is_virtual_user_sender(
            "@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:other.com"
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
            "@_cumments_my-blog_3282F2A21B4A1E6B3282F2A21B4A1E6B:example.com"
        ));
        // Site ids cannot contain underscores.
        assert!(!is_virtual_user_sender(
            "@_cumments_my_blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:example.com"
        ));
        // Non-Cumments senders.
        assert!(!is_virtual_user_sender("@alice:example.com"));
        assert!(!is_virtual_user_sender(
            "_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b"
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
                "reason": "{\"host.curious.cumments.redaction\":{\"site_id\":\"my-blog\",\"target_event_id\":\"$target:hs\",\"submission_id\":7}}",
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
        assert_eq!(parsed.submission_id, Some(7));
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
    fn edit_event_carries_submission_id_for_precise_closed_loop() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$edit:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "body": " * **Alice**: edited",
                "m.new_content": {
                    "body": "**Alice**: edited",
                    "host.curious.cumments.message": {
                        "guest_id": "abcd",
                        "public_key": "pubkey",
                        "signature": "sig",
                        "challenge": "chal",
                        "content": "edited",
                        "displayname": "Alice",
                        "submission_id": 42,
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
        assert_eq!(parsed.display_name.as_deref(), Some("Alice"));
        assert_eq!(parsed.author_public_key.as_deref(), Some("pubkey"));
        assert_eq!(parsed.author_signature.as_deref(), Some("sig"));
        assert!(matches!(&parsed.content, Content::Text(t) if t.body == "edited"));
        assert_eq!(parsed.submission_id, Some(42));
        assert_eq!(
            parsed
                .relates_to
                .as_ref()
                .map(|r| r.target_event_id.as_str()),
            Some("$original:hs")
        );
        assert_eq!(
            parsed
                .relates_to
                .as_ref()
                .and_then(|r| match &r.new_content {
                    Content::Text(t) => Some(t.body.as_str()),
                    _ => None,
                }),
            Some("edited")
        );
    }

    #[test]
    fn new_message_reads_namespaced_content_block() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$comment:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "**Alice**: hello",
                "host.curious.cumments.message": {
                    "guest_id": "abcd",
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                    "displayname": "Alice",
                    "submission_id": 7,
                }
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_message(&event).expect("parse comment");
        assert_eq!(parsed.display_name.as_deref(), Some("Alice"));
        assert_eq!(parsed.author_public_key.as_deref(), Some("pubkey"));
        assert_eq!(parsed.author_signature.as_deref(), Some("sig"));
        assert!(matches!(&parsed.content, Content::Text(t) if t.body == "hello"));
        assert_eq!(parsed.submission_id, Some(7));
        assert!(parsed.relates_to.is_none());
    }

    #[test]
    fn reply_event_parses_standard_relation() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$reply:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string()),
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
                    "guest_id": "abcd",
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                    "displayname": "Alice",
                    "submission_id": 7,
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
                    "guest_id": "abcd",
                    "public_key": "fake-pubkey",
                    "signature": "fake-signature",
                    "challenge": "fake-challenge",
                    "content": "spoofed content",
                    "displayname": "Spoofed",
                    "submission_id": 42,
                }
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_message(&event).expect("parse native message");
        assert!(!parsed.is_virtual_user_sender);
        assert!(matches!(&parsed.content, Content::Text(t) if t.body == "plain body"));
        assert!(parsed.author_public_key.is_none());
        assert!(parsed.author_signature.is_none());
        assert!(parsed.author_challenge.is_none());
        assert!(parsed.display_name.is_none());
        assert!(parsed.submission_id.is_none());
    }

    #[test]
    fn virtual_user_message_without_full_block_is_untrusted() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$guest:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string()),
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
        assert!(parsed.is_virtual_user_sender);
        assert!(parsed.author_public_key.is_none());
        assert!(parsed.author_signature.is_none());
        assert!(parsed.author_challenge.is_none());
        assert!(parsed.submission_id.is_none());
    }

    fn event_with_content(event_type: &str, content: serde_json::Value) -> PushEvent {
        PushEvent {
            event_type: event_type.to_string(),
            event_id: Some("$e:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@alice:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(content),
            redacts: None,
            unsigned: None,
        }
    }

    #[test]
    fn image_message_parses_media_content() {
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({
                "msgtype": "m.image",
                "body": "cat.png",
                "url": "mxc://hs/abc",
                "filename": "cat.png",
                "info": { "mimetype": "image/png", "size": 1024, "width": 100, "height": 80 },
                "thumbnail_url": "mxc://hs/thumb",
                "alt_text": "a cat",
            }),
        );
        let parsed = parse_push_message(&event).expect("parse image");
        match parsed.content {
            Content::Media(media) => {
                assert_eq!(media.kind, MediaKind::Image);
                assert_eq!(media.url, "mxc://hs/abc");
                assert_eq!(media.filename.as_deref(), Some("cat.png"));
                assert_eq!(media.mimetype.as_deref(), Some("image/png"));
                assert_eq!(media.width, Some(100));
                assert_eq!(media.thumbnail_url.as_deref(), Some("mxc://hs/thumb"));
                assert_eq!(media.alt_text.as_deref(), Some("a cat"));
            }
            other => panic!("expected media content, got {other:?}"),
        }
    }

    #[test]
    fn sticker_event_parses_sticker_media() {
        let event = event_with_content(
            "m.sticker",
            serde_json::json!({
                "body": "sticker.png",
                "url": "mxc://hs/sticker",
            }),
        );
        let parsed = parse_push_message(&event).expect("parse sticker");
        assert!(matches!(
            parsed.content,
            Content::Media(MediaContent {
                kind: MediaKind::Sticker,
                ..
            })
        ));
        assert_eq!(parsed.signable_content(), Some("mxc://hs/sticker"));
    }

    #[test]
    fn location_message_parses_geo_uri() {
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({
                "msgtype": "org.matrix.msc3488.location",
                "body": "here",
                "geo_uri": "geo:31.2,121.5",
            }),
        );
        let parsed = parse_push_message(&event).expect("parse location");
        match &parsed.content {
            Content::Location(location) => {
                assert_eq!(location.geo_uri, "geo:31.2,121.5");
                assert_eq!(location.description.as_deref(), Some("here"));
            }
            other => panic!("expected location content, got {other:?}"),
        }
        assert_eq!(parsed.signable_content(), Some("geo:31.2,121.5"));
    }

    #[test]
    fn poll_start_parses_question_and_options() {
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({
                "msgtype": "org.matrix.msc3381.poll.start",
                "body": "best?",
                "org.matrix.msc3381.poll.start": {
                    "question": { "text": "best?" },
                    "answers": [
                        { "id": "1", "org.matrix.msc3381.poll.answer": { "text": "A" } },
                        { "id": "2", "org.matrix.msc3381.poll.answer": { "text": "B" } },
                    ],
                },
            }),
        );
        let parsed = parse_push_message(&event).expect("parse poll");
        match parsed.content {
            Content::Poll(poll) => {
                assert_eq!(poll.question, "best?");
                assert_eq!(poll.options.len(), 2);
                assert_eq!(poll.options[1].id, "2");
                assert_eq!(poll.options[1].text, "B");
            }
            other => panic!("expected poll content, got {other:?}"),
        }
    }

    #[test]
    fn poll_response_is_routed_separately() {
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({
                "msgtype": "org.matrix.msc3381.poll.response",
                "org.matrix.msc3381.poll.response": { "answers": ["1"] },
                "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:hs" },
            }),
        );
        assert!(parse_push_message(&event).is_none());
        let vote = parse_push_poll_vote(&event).expect("parse vote");
        assert_eq!(vote.poll_message_id, "$poll:hs");
        assert_eq!(vote.answer_ids, vec!["1".to_string()]);
    }

    #[test]
    fn reaction_event_parses_annotation() {
        let event = event_with_content(
            "m.reaction",
            serde_json::json!({
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": "$target:hs",
                    "key": "👍",
                }
            }),
        );
        let reaction = parse_push_reaction(&event).expect("parse reaction");
        assert_eq!(reaction.message_event_id, "$target:hs");
        assert_eq!(reaction.key, "👍");
        assert!(!reaction.is_virtual_user_sender);
        assert!(reaction.author_public_key.is_none());
    }

    #[test]
    fn guest_reaction_parses_proof_block() {
        let mut event = event_with_content(
            "m.reaction",
            serde_json::json!({
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": "$target:hs",
                    "key": "👍",
                },
                "host.curious.cumments.message": {
                    "guest_id": "abcd",
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "👍",
                    "displayname": "",
                }
            }),
        );
        event.sender = Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string());
        let reaction = parse_push_reaction(&event).expect("parse guest reaction");
        assert!(reaction.is_virtual_user_sender);
        assert_eq!(reaction.author_public_key.as_deref(), Some("pubkey"));
        assert_eq!(reaction.author_signature.as_deref(), Some("sig"));
        assert_eq!(reaction.author_challenge.as_deref(), Some("chal"));
    }

    #[test]
    fn encrypted_event_parses_placeholder() {
        let event = event_with_content(
            "m.room.encrypted",
            serde_json::json!({
                "algorithm": "m.megolm.v1.aes-sha2",
                "sender_key": "SENDER",
            }),
        );
        let parsed = parse_push_message(&event).expect("parse encrypted");
        match parsed.content {
            Content::Encrypted(encrypted) => {
                assert_eq!(encrypted.algorithm, "m.megolm.v1.aes-sha2");
                assert_eq!(encrypted.sender_key.as_deref(), Some("SENDER"));
            }
            other => panic!("expected encrypted content, got {other:?}"),
        }
    }

    #[test]
    fn unknown_msgtype_degrades_with_body() {
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({ "msgtype": "m.custom", "body": "fallback text" }),
        );
        let parsed = parse_push_message(&event).expect("parse unknown");
        match parsed.content {
            Content::Unknown(unknown) => {
                assert_eq!(unknown.fallback.as_deref(), Some("fallback text"));
            }
            other => panic!("expected unknown content, got {other:?}"),
        }
    }

    #[test]
    fn thread_relation_extracts_thread_root() {
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({
                "msgtype": "m.text",
                "body": "in thread",
                "m.relates_to": {
                    "rel_type": "m.thread",
                    "event_id": "$thread:hs",
                },
            }),
        );
        let parsed = parse_push_message(&event).expect("parse thread message");
        assert_eq!(parsed.thread_root.as_deref(), Some("$thread:hs"));
    }

    #[test]
    fn member_state_event_parses_profile() {
        let event = event_with_content(
            "m.room.member",
            serde_json::json!({
                "membership": "join",
                "displayname": "Alice",
                "avatar_url": "mxc://hs/a",
            }),
        );
        let parsed = parse_push_state(&event).expect("parse member state");
        assert_eq!(parsed.event_type, "m.room.member");
        assert_eq!(parsed.state_key, "");
        assert_eq!(parsed.content["membership"], "join");
    }

    #[test]
    fn room_name_state_event_parses() {
        let event = event_with_content(
            "m.room.name",
            serde_json::json!({ "name": "Comments: my-blog/hello" }),
        );
        let parsed = parse_push_state(&event).expect("parse name state");
        assert_eq!(parsed.event_type, "m.room.name");
        assert_eq!(parsed.content["name"], "Comments: my-blog/hello");
    }
}

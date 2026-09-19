//! Parsing and dispatch of Matrix push events into processor inputs.

use super::types::PushEvent;
use crate::event_processor::EventProcessor;
use crate::parsed::{
    ParsedPollEnd, ParsedPollVote, ParsedReaction, ParsedRelation, ParsedRoomMessage,
    ParsedRoomRedaction, ParsedRoomState, ParsedSpaceChild,
};
use crate::verification::virtual_sender_matches;
use cumments_core::canonical::CanonicalJson;
use cumments_core::models::{
    Content, EncryptedPlaceholder, LocationContent, MediaContent, MediaKind, PollContent,
    PollOption, RoomIdentity, TextContent, TextStyle, UnknownContent,
};
use cumments_core::poll::{
    EndPollWireSemantics, PollAnswerFact, PollSemanticAnswer, PollSemanticKind, PollStartFact,
    PollStatus, PollWireSemantics, VoteWireSemantics, verify_end_poll_proof,
    verify_poll_start_proof, verify_vote_proof,
};
use cumments_core::protocol::{
    MESSAGE_CONTENT_KEY, MESSAGE_SCHEMA_VERSION, PROVENANCE_CONTENT_KEY, PROVENANCE_SCHEMA_VERSION,
    REDACTION_PROOF_KEY,
};
use cumments_matrix::poll::{
    POLL_END_EVENT_TYPE, POLL_RESPONSE_EVENT_TYPE, POLL_START_EVENT_TYPE, PollEndEvent, PollEvent,
    PollKind, PollResponseEvent, PollStartEvent,
};
use tracing::warn;

/// Returns `true` if the Cumments message block's `schema` field is
/// supported. `None` (absent) is unsupported under the v1 break. `1` is current.
/// Any other integer, non-integer, or malformed value is unsupported.
fn message_block_schema_is_supported(block: Option<&serde_json::Value>) -> bool {
    let Some(block) = block else {
        return true;
    };
    match block.get("schema") {
        None => false,
        Some(v) if v.is_i64() && v.as_i64() == Some(MESSAGE_SCHEMA_VERSION) => true,
        Some(v) if v.is_u64() && v.as_u64() == Some(MESSAGE_SCHEMA_VERSION as u64) => true,
        _ => false,
    }
}

/// Map a Matrix wire poll `kind` to its frozen semantic value.
///
/// Custom/unknown wire kinds have no semantic equivalent, so a signed
/// operation can never be proven to match them.
fn semantic_kind_for(kind: &PollKind) -> Option<PollSemanticKind> {
    match kind {
        PollKind::Disclosed => Some(PollSemanticKind::Disclosed),
        PollKind::Undisclosed => Some(PollSemanticKind::Undisclosed),
        PollKind::Other(_) => None,
    }
}

/// Whether a poll event's Cumments provenance block declares the supported
/// schema. Poll proofs live in `host.curious.cumments`, not the message block.
fn provenance_schema_is_supported(content: &serde_json::Value) -> bool {
    content
        .get(PROVENANCE_CONTENT_KEY)
        .and_then(|block| block.get("schema"))
        .and_then(|schema| schema.as_i64())
        == Some(PROVENANCE_SCHEMA_VERSION)
}

fn message_schema_is_supported(content: &serde_json::Value) -> bool {
    // Check primary block (m.room.message, m.reaction, poll response, location)
    let primary_ok = message_block_schema_is_supported(content.get(MESSAGE_CONTENT_KEY));
    if !primary_ok {
        return false;
    }
    // For edits (m.replace), also check m.new_content block if present.
    if let Some(new_content) = content.get("m.new_content") {
        let edit_ok = message_block_schema_is_supported(new_content.get(MESSAGE_CONTENT_KEY));
        if !edit_ok {
            return false;
        }
    }
    true
}

// ── Event dispatch ────────────────────────────────────────────────

/// Route a single push event to the appropriate processor method.
pub(crate) async fn process_single_event(
    event: &PushEvent,
    processor: &EventProcessor,
) -> anyhow::Result<()> {
    let event_type = event.event_type.as_str();

    match event_type {
        "m.room.message" | "m.sticker" | "m.room.encrypted" => {
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
        | "m.room.image_pack"
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
        // The adopted MSC3381 direct event types (not the `m.room.message`
        // wrapper) are parsed through the typed poll layer.
        POLL_START_EVENT_TYPE | POLL_RESPONSE_EVENT_TYPE | POLL_END_EVENT_TYPE => {
            process_poll_event(event, processor).await?;
        }
        _ => {
            // Ignore other event types
        }
    }

    Ok(())
}

// ── Push event helpers ────────────────────────────────────────────

/// Read a string field from a Cumments content block.
///
/// Poll events carry their proof in the provenance block
/// (`host.curious.cumments`); other events use the message block
/// (`host.curious.cumments.message`). Provenance is checked first, and the
/// `m.new_content` replacement payload is checked last.
fn namespaced_string<'a>(content: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    content
        .get(PROVENANCE_CONTENT_KEY)
        .and_then(|ns| ns.get(key))
        .or_else(|| content.get(MESSAGE_CONTENT_KEY).and_then(|ns| ns.get(key)))
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
        .get(PROVENANCE_CONTENT_KEY)
        .and_then(|ns| ns.get(key))
        .or_else(|| content.get(MESSAGE_CONTENT_KEY).and_then(|ns| ns.get(key)))
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
    let Some((site_id, visitor_id)) = rest.rsplit_once('_') else {
        return false;
    };
    let site_ok = !site_id.is_empty()
        && site_id.len() <= 64
        && site_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let guest_ok = visitor_id.len() == 32
        && visitor_id
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f'));
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
    // ignored so it cannot be used to impersonate a visitor identity.
    let author_public_key = namespaced_string(content, "public_key").map(|s| s.to_string());
    let author_signature = namespaced_string(content, "signature").map(|s| s.to_string());
    let author_challenge = namespaced_string(content, "challenge").map(|s| s.to_string());
    let structured_content = namespaced_string(content, "content");

    let mut trusted_block = is_virtual_sender
        && author_public_key.is_some()
        && author_signature.is_some()
        && author_challenge.is_some()
        && structured_content.is_some();
    // Schema compatibility: missing → legacy 1, 1 → current, other → unsupported.
    // Unsupported schema must not enter the trusted visitor path.
    if trusted_block && !message_schema_is_supported(content) {
        trusted_block = false;
    }

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
    // For visitor messages, the structured block takes precedence over the
    // plain-text body (only text messages are signable today).
    if is_virtual_sender
        && let Content::Text(text) = &mut parsed_content
        && let Some(structured) = structured_content
    {
        text.body = structured.to_string();
    }

    // Extract the standard rich-reply and Thread relations, if any.
    // `is_falling_back` distinguishes a genuine direct reply from a
    // fallback-only `m.in_reply_to` that accompanies a Thread. See
    // `misc/design/thread-redesign-0903.md` §5: fallback targets must not be
    // projected as `reply_to`.
    let (reply_to, thread_root) = parse_relations(content);

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
            // For visitor edits, the structured block inside m.new_content still
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
        event_type: event.event_type.clone(),
        sender: sender.clone(),
        content: parsed_content,
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
        width: dimension("w").or_else(|| dimension("width")),
        height: dimension("h").or_else(|| dimension("height")),
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
    let answers: Vec<PollOption> = poll
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
        .take(20)
        .collect();
    let max_selections = poll
        .get("max_selections")
        .and_then(|v| v.as_u64())
        .filter(|value| *value >= 1)
        .unwrap_or(1);
    let kind = match poll.get("kind").and_then(|k| k.as_str()) {
        Some("org.matrix.msc3381.poll.disclosed") => PollSemanticKind::Disclosed,
        _ => PollSemanticKind::Undisclosed,
    };
    Content::Poll(PollContent {
        question,
        answers,
        kind,
        max_selections,
        status: PollStatus::Open,
        end_time: None,
        results: None,
        total_votes: 0,
        responses: Vec::new(),
        my_votes: None,
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
    // For reactions, the trusted block is not gated by structured_content
    // presence in the same way as messages, but schema still applies when
    // a Cumments block is present. Unsupported schema should not be trusted
    // even if sender is virtual.
    let has_cumments_block = content.get(MESSAGE_CONTENT_KEY).is_some();
    let schema_ok = message_block_schema_is_supported(content.get(MESSAGE_CONTENT_KEY));
    let effective_virtual = is_virtual_user_sender && (!has_cumments_block || schema_ok);
    Some(ParsedReaction {
        room_id: room_id.clone(),
        event_id: event_id.clone(),
        sender: sender.clone(),
        message_event_id: message_event_id.to_string(),
        key: key.to_string(),
        origin_server_ts: event.origin_server_ts.unwrap_or(0),
        is_virtual_user_sender: effective_virtual,
        author_public_key: if effective_virtual {
            namespaced_string(content, "public_key").map(|s| s.to_string())
        } else {
            None
        },
        author_signature: if effective_virtual {
            namespaced_string(content, "signature").map(|s| s.to_string())
        } else {
            None
        },
        author_challenge: if effective_virtual {
            namespaced_string(content, "challenge").map(|s| s.to_string())
        } else {
            None
        },
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

/// Extract the standard rich-reply and Thread relations from event content.
///
/// `is_falling_back` distinguishes a genuine direct reply from a fallback-only
/// `m.in_reply_to` that accompanies a Thread; fallback targets must not be
/// projected as `reply_to`.
fn parse_relations(content: &serde_json::Value) -> (Option<String>, Option<String>) {
    let is_falling_back = content
        .get("m.relates_to")
        .and_then(|rel| rel.get("is_falling_back"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reply_to = if is_falling_back {
        None
    } else {
        content
            .get("m.relates_to")
            .and_then(|rel| rel.get("m.in_reply_to"))
            .and_then(|reply| reply.get("event_id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let thread_root = content.get("m.relates_to").and_then(|rel| {
        let rel_type = rel.get("rel_type").and_then(|v| v.as_str())?;
        if rel_type != "m.thread" {
            return None;
        }
        rel.get("event_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    });
    (reply_to, thread_root)
}

// ── Direct MSC3381 poll events ────────────────────────────────────

/// Route one adopted `org.matrix.msc3381.poll.*` direct event into the existing
/// projection pipeline. The typed poll layer validates the wire content; a
/// malformed event is dropped so it cannot corrupt an otherwise valid poll.
async fn process_poll_event(event: &PushEvent, processor: &EventProcessor) -> anyhow::Result<()> {
    let (Some(event_id), Some(sender)) = (event.event_id.as_ref(), event.sender.as_ref()) else {
        return Ok(());
    };
    let Some(content) = event.content.as_ref() else {
        return Ok(());
    };
    let origin_server_ts = event.origin_server_ts.unwrap_or(0);
    let server_name = processor.server_name();
    let parsed = match PollEvent::parse(
        &event.event_type,
        Some(event_id),
        sender,
        origin_server_ts,
        content,
    ) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return Ok(()),
        Err(error) => {
            warn!(
                event_id = %event_id,
                error = %error,
                "Ignoring malformed MSC3381 poll event"
            );
            return Ok(());
        }
    };

    match parsed {
        PollEvent::Start(start) => {
            // Room identity is the target's `site_id` / `page_slug`; it is
            // needed to verify that the signed operation matches the wire
            // event, so resolve it before trusting the start.
            let Some(room_id) = event.room_id.as_deref() else {
                return Ok(());
            };
            let room_identity = processor.resolve_room_identity(room_id).await?;
            let Some(mut message) =
                parse_push_poll_start(event, &start, room_identity.as_ref(), server_name)
            else {
                return Ok(());
            };
            message.room_identity = room_identity;
            processor.process_room_message(message).await?;
        }
        PollEvent::Response(response) => {
            // Room identity supplies the target's `site_id` / `page_slug`, which
            // the wire event does not carry; it is needed to verify a visitor's
            // signed Vote operation.
            let Some(room_id) = event.room_id.as_deref() else {
                return Ok(());
            };
            let room_identity = processor.resolve_room_identity(room_id).await?;
            let Some(mut vote) =
                parse_push_poll_response(event, &response, room_identity.as_ref(), server_name)
            else {
                return Ok(());
            };
            vote.room_identity = room_identity;
            processor.process_poll_vote(vote).await?;
        }
        PollEvent::End(end) => {
            let Some(room_id) = event.room_id.as_deref() else {
                return Ok(());
            };
            let room_identity = processor.resolve_room_identity(room_id).await?;
            let Some(mut parsed_end) =
                parse_push_poll_end(event, &end, room_identity.as_ref(), server_name)
            else {
                return Ok(());
            };
            parsed_end.room_identity = room_identity;
            processor.process_poll_end(parsed_end).await?;
        }
    }
    Ok(())
}

/// Project a direct `poll.start` event as a Poll-backed comment. The same
/// definition rules the reducer applies decide whether the start is valid;
/// an invalid definition never becomes a projected poll.
///
/// `room_identity` supplies the target's `site_id` / `page_slug`, which the
/// wire event does not carry; it is required to prove that a visitor's signed
/// semantic operation is the Poll the event actually encodes. `server_name`
/// binds the event sender to the visitor's public key.
fn parse_push_poll_start(
    event: &PushEvent,
    start: &PollStartEvent,
    room_identity: Option<&RoomIdentity>,
    server_name: Option<&str>,
) -> Option<ParsedRoomMessage> {
    let room_id = event.room_id.as_ref()?;
    let content = event.content.as_ref()?;
    let poll = &start.content.poll;

    let Some(max_selections) = u8::try_from(poll.max_selections).ok() else {
        warn!(
            event_id = ?event.event_id,
            max_selections = poll.max_selections,
            "Ignoring poll start with an unsupported selection limit"
        );
        return None;
    };
    let definition = PollStartFact {
        event_id: event.event_id.clone().unwrap_or_default(),
        sender: start.sender.clone(),
        origin_server_ts: start.origin_server_ts,
        question: poll.question.text.clone(),
        answers: poll
            .answers
            .iter()
            .map(|answer| PollAnswerFact::new(answer.id.clone(), answer.text.clone()))
            .collect(),
        max_selections: poll.max_selections,
        disclosed: poll.kind.is_disclosed(),
        reply_to: None,
        thread_root: None,
    };
    if let Err(error) = definition.validate() {
        warn!(
            event_id = ?event.event_id,
            %error,
            "Ignoring invalid poll start"
        );
        return None;
    }

    let is_virtual_sender = is_virtual_user_sender(&start.sender);
    let author_public_key = namespaced_string(content, "public_key").map(str::to_owned);
    let author_signature = namespaced_string(content, "signature").map(str::to_owned);
    let author_challenge = namespaced_string(content, "challenge").map(str::to_owned);
    let (reply_to, thread_root) = parse_relations(content);

    // Visitor poll starts are authenticated by the frozen semantic-operation
    // envelope in the provenance block, and the signed operation must denote
    // exactly the Poll this wire event encodes. Matrix-native senders carry no
    // Cumments proof.
    let trusted_block = if is_virtual_sender {
        let Some(semantic_kind) = semantic_kind_for(&poll.kind) else {
            warn!(
                event_id = ?event.event_id,
                "Rejecting visitor poll start with a wire kind having no semantic equivalent"
            );
            return None;
        };
        let Some(identity) = room_identity else {
            warn!(
                event_id = ?event.event_id,
                "Rejecting visitor poll start whose target identity cannot be resolved"
            );
            return None;
        };
        // The semantic meaning the wire event actually carries.
        let wire_semantics = PollWireSemantics {
            site_id: identity.site_id.clone(),
            page_slug: identity.page_slug.clone(),
            reply_to: reply_to.clone(),
            thread_root: thread_root.clone(),
            question: poll.question.text.clone(),
            answers: poll
                .answers
                .iter()
                .map(|answer| PollSemanticAnswer::new(answer.id.clone(), answer.text.clone()))
                .collect(),
            kind: semantic_kind,
            max_selections: poll.max_selections,
        };
        let operation_id = namespaced_string(content, "operation_id");
        let signed_operation = content
            .get(PROVENANCE_CONTENT_KEY)
            .and_then(|provenance| provenance.get("content"));
        let valid = match (
            author_public_key.as_deref(),
            author_signature.as_deref(),
            author_challenge.as_deref(),
            operation_id,
            signed_operation,
        ) {
            (Some(pk), Some(sig), Some(chal), Some(operation_id), Some(op_json))
                if provenance_schema_is_supported(content) =>
            {
                // The proof only counts when the event was sent by the virtual
                // user derived from `pk`; the signature alone is not identity.
                virtual_sender_matches(server_name, &start.sender, &identity.site_id, pk)
                    && CanonicalJson::from_json_value(op_json).is_some_and(|operation| {
                        verify_poll_start_proof(
                            pk,
                            &operation,
                            operation_id,
                            chal,
                            sig,
                            &wire_semantics,
                        )
                    })
            }
            _ => false,
        };
        if !valid {
            warn!(
                event_id = ?event.event_id,
                "Rejecting visitor poll start whose sender or signed operation does not match its wire content"
            );
            return None;
        }
        true
    } else {
        false
    };

    Some(ParsedRoomMessage {
        room_id: room_id.clone(),
        event_id: event.event_id.clone().unwrap_or_default(),
        event_type: event.event_type.clone(),
        sender: start.sender.clone(),
        content: Content::Poll(PollContent {
            question: poll.question.text.clone(),
            answers: poll
                .answers
                .iter()
                .map(|answer| PollOption {
                    id: answer.id.clone(),
                    text: answer.text.clone(),
                })
                .collect(),
            kind: semantic_kind_for(&poll.kind).unwrap_or(PollSemanticKind::Undisclosed),
            max_selections: u64::from(max_selections),
            status: PollStatus::Open,
            end_time: None,
            results: None,
            total_votes: 0,
            responses: Vec::new(),
            my_votes: None,
        }),
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
        origin_server_ts: start.origin_server_ts,
        relates_to: None,
        room_identity: None,
        raw_content: content.clone(),
    })
}

/// Parse a direct `poll.response` event into a [`ParsedPollVote`].
///
/// `room_identity` supplies the target's `site_id` / `page_slug`, which the
/// wire event does not carry; it is required to prove that a visitor's signed
/// VOTE operation is the selection set the event actually encodes.
/// `server_name` binds the event sender to the visitor's public key.
fn parse_push_poll_response(
    event: &PushEvent,
    response: &PollResponseEvent,
    room_identity: Option<&RoomIdentity>,
    server_name: Option<&str>,
) -> Option<ParsedPollVote> {
    let room_id = event.room_id.as_ref()?;
    let content = event.content.as_ref()?;
    let is_virtual_sender = is_virtual_user_sender(&response.sender);

    // Visitor responses are authenticated by the frozen VOTE envelope in the
    // provenance block, and the signed operation must denote exactly the
    // selections this wire event carries. Matrix-native senders carry no
    // Cumments proof.
    let (author_public_key, author_signature, author_challenge, trusted) = if is_virtual_sender {
        let pk = namespaced_string(content, "public_key").map(str::to_owned);
        let sig = namespaced_string(content, "signature").map(str::to_owned);
        let chal = namespaced_string(content, "challenge").map(str::to_owned);
        let operation_id = namespaced_string(content, "operation_id");
        let signed = content
            .get(PROVENANCE_CONTENT_KEY)
            .and_then(|provenance| provenance.get("content"));
        let valid = match (&pk, &sig, &chal, operation_id, signed, room_identity) {
            (
                Some(pk),
                Some(sig),
                Some(chal),
                Some(operation_id),
                Some(op_json),
                Some(identity),
            ) if provenance_schema_is_supported(content) => {
                // Selections are an unordered set: normalize the wire answers
                // before comparing them to the signed operation.
                let mut option_ids = response.content.response.answers.clone();
                option_ids.sort();
                option_ids.dedup();
                let wire = VoteWireSemantics {
                    site_id: identity.site_id.clone(),
                    page_slug: identity.page_slug.clone(),
                    poll_event_id: response.content.relates_to.event_id.clone(),
                    option_ids,
                };
                CanonicalJson::from_json_value(op_json).is_some_and(|op| {
                    // The proof only counts when the event was sent by the
                    // virtual user derived from `pk`.
                    virtual_sender_matches(server_name, &response.sender, &identity.site_id, pk)
                        && verify_vote_proof(pk, &op, operation_id, chal, sig, &wire)
                })
            }
            _ => false,
        };
        if !valid {
            warn!(
                event_id = ?event.event_id,
                "Rejecting visitor poll response whose sender or signed operation does not match its wire content"
            );
            return None;
        }
        (pk, sig, chal, true)
    } else {
        (None, None, None, false)
    };

    Some(ParsedPollVote {
        room_id: room_id.clone(),
        event_id: response.event_id.clone().unwrap_or_default(),
        sender: response.sender.clone(),
        poll_message_id: response.content.relates_to.event_id.clone(),
        answer_ids: response.content.response.answers.clone(),
        origin_server_ts: response.origin_server_ts,
        is_virtual_user_sender: is_virtual_sender,
        author_public_key: if trusted { author_public_key } else { None },
        author_signature: if trusted { author_signature } else { None },
        author_challenge: if trusted { author_challenge } else { None },
        room_identity: None,
    })
}

/// Parse a direct `poll.end` event into a [`ParsedPollEnd`].
///
/// `room_identity` supplies the target's `site_id` / `page_slug`, which the
/// wire event does not carry; it is required to prove that a visitor's signed
/// END_POLL operation is the target this wire event encodes. `server_name`
/// binds the event sender to the visitor's public key.
fn parse_push_poll_end(
    event: &PushEvent,
    end: &PollEndEvent,
    room_identity: Option<&RoomIdentity>,
    server_name: Option<&str>,
) -> Option<ParsedPollEnd> {
    let room_id = event.room_id.as_ref()?;
    let content = event.content.as_ref()?;
    let is_virtual_sender = is_virtual_user_sender(&end.sender);

    // Visitor ends are authenticated by the frozen END_POLL envelope in the
    // provenance block, and the signed operation must denote exactly the
    // target this wire event carries. Matrix-native senders carry no
    // Cumments proof.
    let (author_public_key, author_signature, author_challenge, trusted) = if is_virtual_sender {
        let pk = namespaced_string(content, "public_key").map(str::to_owned);
        let sig = namespaced_string(content, "signature").map(str::to_owned);
        let chal = namespaced_string(content, "challenge").map(str::to_owned);
        let operation_id = namespaced_string(content, "operation_id");
        let signed = content
            .get(PROVENANCE_CONTENT_KEY)
            .and_then(|provenance| provenance.get("content"));
        let valid = match (&pk, &sig, &chal, operation_id, signed, room_identity) {
            (
                Some(pk),
                Some(sig),
                Some(chal),
                Some(operation_id),
                Some(op_json),
                Some(identity),
            ) if provenance_schema_is_supported(content) => {
                let wire = EndPollWireSemantics {
                    site_id: identity.site_id.clone(),
                    page_slug: identity.page_slug.clone(),
                    poll_event_id: end.content.relates_to.event_id.clone(),
                };
                CanonicalJson::from_json_value(op_json).is_some_and(|op| {
                    // The proof only counts when the event was sent by the
                    // virtual user derived from `pk`.
                    virtual_sender_matches(server_name, &end.sender, &identity.site_id, pk)
                        && verify_end_poll_proof(pk, &op, operation_id, chal, sig, &wire)
                })
            }
            _ => false,
        };
        if !valid {
            warn!(
                event_id = ?event.event_id,
                "Rejecting visitor poll end whose sender or signed operation does not match its wire content"
            );
            return None;
        }
        (pk, sig, chal, true)
    } else {
        (None, None, None, false)
    };

    Some(ParsedPollEnd {
        room_id: room_id.clone(),
        event_id: end.event_id.clone().unwrap_or_default(),
        sender: end.sender.clone(),
        poll_message_id: end.content.relates_to.event_id.clone(),
        origin_server_ts: end.origin_server_ts,
        is_virtual_user_sender: is_virtual_sender,
        author_public_key: if trusted { author_public_key } else { None },
        author_signature: if trusted { author_signature } else { None },
        author_challenge: if trusted { author_challenge } else { None },
        room_identity: None,
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
                        "schema": 1,
                        "public_key": "pubkey",
                        "signature": "sig",
                        "challenge": "chal",
                        "content": "edited",
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
                    "schema": 1,
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                    "submission_id": 7,
                }
            })),
            redacts: None,
            unsigned: None,
        };

        let parsed = parse_push_message(&event).expect("parse comment");
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
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
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
                    "public_key": "fake-pubkey",
                    "signature": "fake-signature",
                    "challenge": "fake-challenge",
                    "content": "spoofed content",
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
        assert!(parsed.submission_id.is_none());
    }

    #[test]
    fn virtual_user_message_without_full_block_is_untrusted() {
        let event = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$visitor:hs".to_string()),
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

        let parsed = parse_push_message(&event).expect("parse visitor message");
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
                assert_eq!(poll.answers.len(), 2);
                assert_eq!(poll.max_selections, 1);
                assert_eq!(poll.answers[1].id, "2");
                assert_eq!(poll.answers[1].text, "B");
            }
            other => panic!("expected poll content, got {other:?}"),
        }
    }

    #[test]
    fn legacy_poll_response_message_is_ignored() {
        // The discarded legacy `m.room.message` poll-response wire is no longer
        // ingested; only the direct `org.matrix.msc3381.poll.response` type is.
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({
                "msgtype": "org.matrix.msc3381.poll.response",
                "org.matrix.msc3381.poll.response": { "answers": ["1"] },
                "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:hs" },
            }),
        );
        assert!(parse_push_message(&event).is_none());
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
    fn visitor_reaction_parses_proof_block() {
        let mut event = event_with_content(
            "m.reaction",
            serde_json::json!({
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": "$target:hs",
                    "key": "👍",
                },
                "host.curious.cumments.message": {
                    "schema": 1,
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "👍",
                }
            }),
        );
        event.sender = Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string());
        let reaction = parse_push_reaction(&event).expect("parse visitor reaction");
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
        assert!(
            parsed.reply_to.is_none(),
            "thread membership alone must not imply a direct reply"
        );
    }

    #[test]
    fn fallback_only_in_reply_to_is_not_projected_as_reply_to() {
        // Thread events emitted through the reply fallback carry an
        // `m.in_reply_to` that points at a fallback target, not the genuine
        // direct parent; `is_falling_back` marks it as such.
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({
                "msgtype": "m.text",
                "body": "in thread via fallback",
                "m.relates_to": {
                    "rel_type": "m.thread",
                    "event_id": "$thread:hs",
                    "m.in_reply_to": { "event_id": "$fallback:hs" },
                    "is_falling_back": true,
                },
            }),
        );
        let parsed = parse_push_message(&event).expect("parse fallback thread message");
        assert_eq!(
            parsed.thread_root.as_deref(),
            Some("$thread:hs"),
            "fallback must not affect Thread membership"
        );
        assert_eq!(
            parsed.reply_to, None,
            "a fallback-only m.in_reply_to must not become reply_to"
        );
    }

    #[test]
    fn genuine_in_thread_reply_projects_both_relations() {
        let event = event_with_content(
            "m.room.message",
            serde_json::json!({
                "msgtype": "m.text",
                "body": "genuine reply",
                "m.relates_to": {
                    "rel_type": "m.thread",
                    "event_id": "$thread:hs",
                    "m.in_reply_to": { "event_id": "$parent:hs" },
                    "is_falling_back": false,
                },
            }),
        );
        let parsed = parse_push_message(&event).expect("parse in-thread reply");
        assert_eq!(parsed.thread_root.as_deref(), Some("$thread:hs"));
        assert_eq!(parsed.reply_to.as_deref(), Some("$parent:hs"));
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

    #[test]
    fn message_schema_legacy_and_1_accepted_2_rejected() {
        // Legacy without schema → unsupported under v1 break (untrusted)
        let legacy = PushEvent {
            event_type: "m.room.message".to_string(),
            event_id: Some("$legacy:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string()),
            origin_server_ts: Some(1000),
            state_key: None,
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "hello",
                "host.curious.cumments.message": {
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                }
            })),
            redacts: None,
            unsigned: None,
        };
        let parsed = parse_push_message(&legacy).expect("parse legacy");
        // Legacy without schema now untrusted
        assert!(parsed.author_public_key.is_none());
        assert!(parsed.author_signature.is_none());
        assert!(parsed.submission_id.is_none());

        // Schema 1 → accepted, extra fields ignored
        let with_schema = PushEvent {
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "hello",
                "host.curious.cumments.message": {
                    "schema": 1,
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                    "extra": "ignore-me",
                }
            })),
            ..legacy.clone()
        };
        let parsed = parse_push_message(&with_schema).expect("parse schema1");
        assert_eq!(parsed.author_public_key.as_deref(), Some("pubkey"));

        // Schema 2 → unsupported → untrusted (author_* cleared)
        let with_schema2 = PushEvent {
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "hello",
                "host.curious.cumments.message": {
                    "schema": 2,
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "hello",
                }
            })),
            ..legacy.clone()
        };
        let parsed = parse_push_message(&with_schema2).expect("parse schema2");
        assert!(parsed.author_public_key.is_none());
        assert!(parsed.author_signature.is_none());
        assert!(parsed.submission_id.is_none());
    }

    #[test]
    fn reaction_schema_2_is_untrusted() {
        let mut legacy = event_with_content(
            "m.reaction",
            serde_json::json!({
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": "$target:hs",
                    "key": "👍",
                },
                "host.curious.cumments.message": {
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "👍",
                }
            }),
        );
        legacy.sender = Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string());
        let parsed = parse_push_reaction(&legacy).expect("legacy reaction");
        assert!(!parsed.is_virtual_user_sender);

        let mut with_schema2 = event_with_content(
            "m.reaction",
            serde_json::json!({
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": "$target:hs",
                    "key": "👍",
                },
                "host.curious.cumments.message": {
                    "schema": 2,
                    "public_key": "pubkey",
                    "signature": "sig",
                    "challenge": "chal",
                    "content": "👍",
                }
            }),
        );
        with_schema2.sender =
            Some("@_cumments_my-blog_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs".to_string());
        let parsed = parse_push_reaction(&with_schema2).expect("schema2 reaction");
        assert!(!parsed.is_virtual_user_sender);
        assert!(parsed.author_public_key.is_none());
    }

    // ── Direct MSC3381 poll events ────────────────────────────────

    fn direct_event(event_type: &str, content: serde_json::Value) -> PushEvent {
        PushEvent {
            event_type: event_type.to_string(),
            event_id: Some("$e:hs".to_string()),
            room_id: Some("!room:hs".to_string()),
            sender: Some("@alice:hs".to_string()),
            origin_server_ts: Some(100),
            state_key: None,
            content: Some(content),
            redacts: None,
            unsigned: None,
        }
    }

    fn poll_start_wire() -> serde_json::Value {
        serde_json::json!({
            "org.matrix.msc1767.text": "best?",
            "org.matrix.msc3381.poll.start": {
                "kind": "org.matrix.msc3381.poll.disclosed",
                "max_selections": 2,
                "question": { "org.matrix.msc1767.text": "best?" },
                "answers": [
                    { "id": "a", "org.matrix.msc1767.text": "A" },
                    { "id": "b", "org.matrix.msc1767.text": "B" },
                ],
            },
        })
    }

    #[test]
    fn direct_poll_start_projects_into_poll_content() {
        let event = direct_event(POLL_START_EVENT_TYPE, poll_start_wire());
        let parsed = PollEvent::parse(
            POLL_START_EVENT_TYPE,
            Some("$e:hs"),
            "@alice:hs",
            100,
            event.content.as_ref().unwrap(),
        )
        .expect("typed parse")
        .expect("is a poll");
        let PollEvent::Start(start) = parsed else {
            panic!("expected start");
        };
        let message = parse_push_poll_start(
            &event,
            &start,
            Some(&poll_identity()),
            Some(TEST_SERVER_NAME),
        )
        .expect("project start");
        match message.content {
            Content::Poll(poll) => {
                assert_eq!(poll.question, "best?");
                assert_eq!(poll.max_selections, 2);
                assert_eq!(poll.answers.len(), 2);
                assert_eq!(poll.answers[0].id, "a");
                assert_eq!(poll.answers[1].id, "b");
            }
            other => panic!("expected poll content, got {other:?}"),
        }
        assert!(!message.is_virtual_user_sender);
        assert_eq!(message.event_type, POLL_START_EVENT_TYPE);
    }

    #[test]
    fn direct_poll_start_carries_thread_relation() {
        let mut content = poll_start_wire();
        content["m.relates_to"] = serde_json::json!({
            "rel_type": "m.thread",
            "event_id": "$thread:hs",
        });
        let event = direct_event(POLL_START_EVENT_TYPE, content);
        let PollEvent::Start(start) = PollEvent::parse(
            POLL_START_EVENT_TYPE,
            Some("$e:hs"),
            "@alice:hs",
            100,
            event.content.as_ref().unwrap(),
        )
        .unwrap()
        .unwrap() else {
            panic!("expected start");
        };
        let message = parse_push_poll_start(
            &event,
            &start,
            Some(&poll_identity()),
            Some(TEST_SERVER_NAME),
        )
        .expect("project start");
        assert_eq!(message.thread_root.as_deref(), Some("$thread:hs"));
        assert!(message.reply_to.is_none());
    }

    #[test]
    fn direct_poll_start_with_invalid_definition_is_rejected() {
        // Duplicate answer ids violate the poll definition rules.
        let content = serde_json::json!({
            "org.matrix.msc1767.text": "best?",
            "org.matrix.msc3381.poll.start": {
                "question": { "org.matrix.msc1767.text": "best?" },
                "answers": [
                    { "id": "a", "org.matrix.msc1767.text": "A" },
                    { "id": "a", "org.matrix.msc1767.text": "A again" },
                ],
            },
        });
        let event = direct_event(POLL_START_EVENT_TYPE, content);
        let PollEvent::Start(start) = PollEvent::parse(
            POLL_START_EVENT_TYPE,
            Some("$e:hs"),
            "@alice:hs",
            100,
            event.content.as_ref().unwrap(),
        )
        .unwrap()
        .unwrap() else {
            panic!("expected start");
        };
        assert!(
            parse_push_poll_start(
                &event,
                &start,
                Some(&poll_identity()),
                Some(TEST_SERVER_NAME)
            )
            .is_none()
        );
    }

    #[test]
    fn direct_poll_response_parses_selections_and_reference() {
        let event = direct_event(
            POLL_RESPONSE_EVENT_TYPE,
            serde_json::json!({
                "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:hs" },
                "org.matrix.msc3381.poll.response": { "answers": ["a", "b"] },
            }),
        );
        let PollEvent::Response(response) = PollEvent::parse(
            POLL_RESPONSE_EVENT_TYPE,
            Some("$e:hs"),
            "@bob:hs",
            100,
            event.content.as_ref().unwrap(),
        )
        .unwrap()
        .unwrap() else {
            panic!("expected response");
        };
        let vote = parse_push_poll_response(
            &event,
            &response,
            Some(&poll_identity()),
            Some(TEST_SERVER_NAME),
        )
        .expect("project response");
        assert_eq!(vote.poll_message_id, "$poll:hs");
        assert_eq!(vote.answer_ids, vec!["a".to_string(), "b".to_string()]);
        assert!(!vote.is_virtual_user_sender);
    }

    #[test]
    fn visitor_poll_response_requires_matching_signed_operation() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use cumments_core::poll::{poll_signature_envelope, vote_semantic_operation};
        use ed25519_dalek::Signer;

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[32u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let identity = poll_identity();

        let build = |answers: &[&str], signed_answers: &[&str]| -> PushEvent {
            let signed: Vec<String> = signed_answers.iter().map(|s| s.to_string()).collect();
            let op = vote_semantic_operation(
                &identity.site_id,
                &identity.page_slug,
                "$poll:hs",
                &signed,
            );
            let envelope = poll_signature_envelope(&op, "vote-op", "chal");
            let signature = URL_SAFE_NO_PAD.encode(
                signing_key
                    .sign(envelope.to_canonical_bytes().as_slice())
                    .to_bytes(),
            );
            let mut event = direct_event(
                POLL_RESPONSE_EVENT_TYPE,
                serde_json::json!({
                    "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:hs" },
                    "org.matrix.msc3381.poll.response": { "answers": answers },
                    PROVENANCE_CONTENT_KEY: {
                        "schema": PROVENANCE_SCHEMA_VERSION,
                        "operation_id": "vote-op",
                        "public_key": public_key,
                        "signature": signature,
                        "challenge": "chal",
                        "content": op.to_json_value(),
                    },
                }),
            );
            event.sender = Some(virtual_sender(&public_key, "my-blog"));
            event
        };

        let parse = |event: &PushEvent,
                     identity: Option<&RoomIdentity>,
                     server_name: Option<&str>|
         -> Option<ParsedPollVote> {
            let content = event.content.as_ref().expect("content");
            let PollEvent::Response(response) = PollEvent::parse(
                POLL_RESPONSE_EVENT_TYPE,
                Some("$e:hs"),
                event.sender.as_deref().unwrap(),
                100,
                content,
            )
            .unwrap()
            .unwrap() else {
                panic!("expected response");
            };
            parse_push_poll_response(event, &response, identity, server_name)
        };

        // Valid: the wire set matches the signed set (order-insensitive).
        let valid = parse(
            &build(&["b", "a"], &["a", "b"]),
            Some(&identity),
            Some(TEST_SERVER_NAME),
        )
        .expect("valid visitor response");
        assert!(valid.is_virtual_user_sender);
        assert_eq!(valid.answer_ids, vec!["b".to_string(), "a".to_string()]);

        // Forged: signature is over ["a"] but the wire claims ["b"].
        assert!(
            parse(
                &build(&["b"], &["a"]),
                Some(&identity),
                Some(TEST_SERVER_NAME)
            )
            .is_none()
        );

        // Without room identity a visitor response cannot be authenticated.
        assert!(parse(&build(&["a"], &["a"]), None, Some(TEST_SERVER_NAME)).is_none());

        // The proof is valid for the embedded key, but the event was sent by
        // the virtual user of a different key: identity binding must reject it.
        let mut foreign = build(&["a"], &["a"]);
        let other_public_key = URL_SAFE_NO_PAD.encode(
            ed25519_dalek::SigningKey::from_bytes(&[34u8; 32])
                .verifying_key()
                .to_bytes(),
        );
        foreign.sender = Some(virtual_sender(&other_public_key, "my-blog"));
        assert!(parse(&foreign, Some(&identity), Some(TEST_SERVER_NAME)).is_none());

        // Without a configured server name there is no sender to bind to.
        assert!(parse(&build(&["a"], &["a"]), Some(&identity), None).is_none());
    }

    #[test]
    fn direct_poll_end_parses_reference() {
        let event = direct_event(
            POLL_END_EVENT_TYPE,
            serde_json::json!({
                "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:hs" },
                "org.matrix.msc1767.text": "The poll has closed.",
                "org.matrix.msc3381.poll.end": {},
            }),
        );
        let PollEvent::End(end) = PollEvent::parse(
            POLL_END_EVENT_TYPE,
            Some("$e:hs"),
            "@alice:hs",
            100,
            event.content.as_ref().unwrap(),
        )
        .unwrap()
        .unwrap() else {
            panic!("expected end");
        };
        let parsed =
            parse_push_poll_end(&event, &end, None, Some(TEST_SERVER_NAME)).expect("project end");
        assert_eq!(parsed.poll_message_id, "$poll:hs");
        assert_eq!(parsed.sender, "@alice:hs");
        assert_eq!(parsed.origin_server_ts, 100);
        assert!(!parsed.is_virtual_user_sender);
    }

    #[test]
    fn visitor_poll_end_requires_matching_signed_operation() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use cumments_core::poll::{end_poll_semantic_operation, poll_signature_envelope};
        use ed25519_dalek::Signer;

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[33u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let identity = poll_identity();

        let build = |poll_event_id: &str, signed_poll_event_id: &str| -> PushEvent {
            let op = end_poll_semantic_operation(
                &identity.site_id,
                &identity.page_slug,
                signed_poll_event_id,
            );
            let envelope = poll_signature_envelope(&op, "end-op", "chal");
            let signature = URL_SAFE_NO_PAD.encode(
                signing_key
                    .sign(envelope.to_canonical_bytes().as_slice())
                    .to_bytes(),
            );
            let mut event = direct_event(
                POLL_END_EVENT_TYPE,
                serde_json::json!({
                    "m.relates_to": { "rel_type": "m.reference", "event_id": poll_event_id },
                    "org.matrix.msc1767.text": "The poll has closed.",
                    "org.matrix.msc3381.poll.end": {},
                    PROVENANCE_CONTENT_KEY: {
                        "schema": PROVENANCE_SCHEMA_VERSION,
                        "operation_id": "end-op",
                        "public_key": public_key,
                        "signature": signature,
                        "challenge": "chal",
                        "content": op.to_json_value(),
                    },
                }),
            );
            event.sender = Some(virtual_sender(&public_key, "my-blog"));
            event
        };

        let parse = |event: &PushEvent,
                     identity: Option<&RoomIdentity>,
                     server_name: Option<&str>|
         -> Option<ParsedPollEnd> {
            let content = event.content.as_ref().expect("content");
            let PollEvent::End(end) = PollEvent::parse(
                POLL_END_EVENT_TYPE,
                Some("$e:hs"),
                event.sender.as_deref().unwrap(),
                100,
                content,
            )
            .unwrap()
            .unwrap() else {
                panic!("expected end");
            };
            parse_push_poll_end(event, &end, identity, server_name)
        };

        // Valid: wire target matches signed target.
        let valid = parse(
            &build("$poll:hs", "$poll:hs"),
            Some(&identity),
            Some(TEST_SERVER_NAME),
        )
        .expect("valid visitor poll end");
        assert!(valid.is_virtual_user_sender);
        assert_eq!(valid.poll_message_id, "$poll:hs");
        assert!(valid.author_public_key.is_some());

        // Forged target: signature is for $poll:hs but wire event targets $other:hs.
        assert!(
            parse(
                &build("$other:hs", "$poll:hs"),
                Some(&identity),
                Some(TEST_SERVER_NAME)
            )
            .is_none()
        );

        // Without room identity, visitor poll end cannot be authenticated.
        assert!(parse(&build("$poll:hs", "$poll:hs"), None, Some(TEST_SERVER_NAME)).is_none());

        // The proof is valid for the embedded key, but the event was sent by
        // the virtual user of a different key: identity binding must reject it.
        let mut foreign = build("$poll:hs", "$poll:hs");
        let other_public_key = URL_SAFE_NO_PAD.encode(
            ed25519_dalek::SigningKey::from_bytes(&[35u8; 32])
                .verifying_key()
                .to_bytes(),
        );
        foreign.sender = Some(virtual_sender(&other_public_key, "my-blog"));
        assert!(parse(&foreign, Some(&identity), Some(TEST_SERVER_NAME)).is_none());

        // Without a configured server name there is no sender to bind to.
        assert!(parse(&build("$poll:hs", "$poll:hs"), Some(&identity), None).is_none());
    }

    #[test]
    fn malformed_direct_poll_event_is_reported_not_reinterpreted() {
        // The reference relation is required; without it the typed layer
        // rejects the event instead of manufacturing a poll fact.
        let content = serde_json::json!({
            "org.matrix.msc3381.poll.end": {},
            "org.matrix.msc1767.text": "closed",
        });
        assert!(
            PollEvent::parse(
                POLL_END_EVENT_TYPE,
                Some("$e:hs"),
                "@alice:hs",
                100,
                &content,
            )
            .is_err()
        );
    }

    /// The server name the poll test events are sent from.
    const TEST_SERVER_NAME: &str = "hs";

    /// The room identity the visitor poll start is authored under.
    fn poll_identity() -> RoomIdentity {
        RoomIdentity {
            site_id: "my-blog".to_string(),
            page_slug: "hello".to_string(),
        }
    }

    /// The deterministic virtual MXID Cumments derives for `public_key` on
    /// `site_id` under the test server name.
    fn virtual_sender(public_key: &str, site_id: &str) -> String {
        let visitor_id = cumments_core::identity::derive_visitor_id_from_public_key(public_key)
            .expect("visitor id");
        format!("@_cumments_{}_{}:{}", site_id, visitor_id, TEST_SERVER_NAME)
    }

    /// Build a visitor-authored direct `poll.start` push event whose signed
    /// provenance and wire content agree.
    fn visitor_poll_start_event() -> PushEvent {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use cumments_core::poll::{
            PollSemanticAnswer, PollSemanticKind, poll_semantic_operation, poll_signature_envelope,
        };
        use ed25519_dalek::Signer;

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let answers = vec![
            PollSemanticAnswer::new("a", "A"),
            PollSemanticAnswer::new("b", "B"),
        ];
        let operation = poll_semantic_operation(
            "my-blog",
            "hello",
            None,
            None,
            "q?",
            &answers,
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("validated definition");
        let operation_id = "op-projection-1";
        let challenge = "chal";
        let envelope = poll_signature_envelope(&operation, operation_id, challenge);
        let signature = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(envelope.to_canonical_bytes().as_slice())
                .to_bytes(),
        );

        let content = serde_json::json!({
            "org.matrix.msc1767.text": "q?",
            "org.matrix.msc3381.poll.start": {
                "kind": "org.matrix.msc3381.poll.disclosed",
                "max_selections": 1,
                "question": { "org.matrix.msc1767.text": "q?" },
                "answers": [
                    { "id": "a", "org.matrix.msc1767.text": "A" },
                    { "id": "b", "org.matrix.msc1767.text": "B" },
                ],
            },
            "host.curious.cumments": {
                "schema": 1,
                "operation_id": operation_id,
                "public_key": public_key,
                "signature": signature,
                "challenge": challenge,
                "content": operation.to_json_value(),
            },
        });
        let mut event = direct_event(POLL_START_EVENT_TYPE, content);
        event.sender = Some(virtual_sender(&public_key, "my-blog"));
        event
    }

    /// Run the start projection with the authored room identity.
    fn project(event: &PushEvent, identity: Option<&RoomIdentity>) -> Option<ParsedRoomMessage> {
        project_with_server(event, identity, Some(TEST_SERVER_NAME))
    }

    /// Run the start projection with an explicit configured server name.
    fn project_with_server(
        event: &PushEvent,
        identity: Option<&RoomIdentity>,
        server_name: Option<&str>,
    ) -> Option<ParsedRoomMessage> {
        let PollEvent::Start(start) = PollEvent::parse(
            POLL_START_EVENT_TYPE,
            event.event_id.as_deref(),
            event.sender.as_deref().unwrap(),
            event.origin_server_ts.unwrap(),
            event.content.as_ref().unwrap(),
        )
        .expect("typed parse")
        .expect("is a poll") else {
            panic!("expected start");
        };
        parse_push_poll_start(event, &start, identity, server_name)
    }

    /// Mutable access to the wire `poll.start` block.
    fn wire_poll(event: &mut PushEvent) -> &mut serde_json::Value {
        &mut event.content.as_mut().unwrap()["org.matrix.msc3381.poll.start"]
    }

    /// Mutable access to the signed Cumments provenance block.
    fn wire_provenance(event: &mut PushEvent) -> &mut serde_json::Value {
        &mut event.content.as_mut().unwrap()["host.curious.cumments"]
    }

    type Tamper = fn(&mut PushEvent);

    fn tamper_question(event: &mut PushEvent) {
        wire_poll(event)["question"]["org.matrix.msc1767.text"] = serde_json::json!("evil?");
    }

    fn tamper_answer_id(event: &mut PushEvent) {
        wire_poll(event)["answers"][0]["id"] = serde_json::json!("z");
    }

    fn tamper_answer_text(event: &mut PushEvent) {
        wire_poll(event)["answers"][0]["org.matrix.msc1767.text"] = serde_json::json!("Z");
    }

    fn tamper_answer_order(event: &mut PushEvent) {
        wire_poll(event)["answers"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
    }

    fn tamper_kind(event: &mut PushEvent) {
        wire_poll(event)["kind"] = serde_json::json!("org.matrix.msc3381.poll.undisclosed");
    }

    fn tamper_custom_kind(event: &mut PushEvent) {
        wire_poll(event)["kind"] = serde_json::json!("com.example.poll.secret");
    }

    fn tamper_max_selections(event: &mut PushEvent) {
        wire_poll(event)["max_selections"] = serde_json::json!(2);
    }

    fn tamper_thread_relation(event: &mut PushEvent) {
        event.content.as_mut().unwrap()["m.relates_to"] = serde_json::json!({
            "rel_type": "m.thread",
            "event_id": "$thread:hs",
        });
    }

    fn tamper_provenance_content(event: &mut PushEvent) {
        wire_provenance(event)["content"] = serde_json::json!([
            "POLL",
            ["my-blog", "hello", null, null],
            ["evil?", [["a", "A"], ["b", "B"]], "disclosed", 1],
            1
        ]);
    }

    fn tamper_operation_id(event: &mut PushEvent) {
        wire_provenance(event)["operation_id"] = serde_json::json!("op-other");
    }

    fn tamper_challenge(event: &mut PushEvent) {
        wire_provenance(event)["challenge"] = serde_json::json!("other-chal");
    }

    fn tamper_signature(event: &mut PushEvent) {
        wire_provenance(event)["signature"] = serde_json::json!("AAAA");
    }

    #[test]
    fn visitor_poll_start_with_consistent_provenance_is_trusted() {
        let event = visitor_poll_start_event();
        let parsed = project(&event, Some(&poll_identity())).expect("project visitor poll");
        assert!(parsed.is_virtual_user_sender);
        assert!(parsed.author_public_key.is_some());
        assert!(parsed.author_signature.is_some());
        assert!(parsed.author_challenge.is_some());
        assert!(matches!(parsed.content, Content::Poll(_)));
    }

    #[test]
    fn visitor_poll_start_with_unknown_identity_is_rejected() {
        let event = visitor_poll_start_event();
        assert!(
            project(&event, None).is_none(),
            "a visitor poll cannot be verified without its target identity"
        );
    }

    #[test]
    fn visitor_poll_start_with_mismatched_target_is_rejected() {
        let event = visitor_poll_start_event();
        // The signed operation names a different page than the room resolves to.
        let other = RoomIdentity {
            site_id: "my-blog".to_string(),
            page_slug: "other".to_string(),
        };
        assert!(
            project(&event, Some(&other)).is_none(),
            "a signed target that differs from the event's room must be rejected"
        );
    }

    #[test]
    fn visitor_poll_start_with_foreign_sender_is_rejected() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

        // The embedded proof is genuinely valid for its own key; the event is
        // then sent by the virtual user of a *different* key, so only the
        // sender/public-key binding fails.
        let mut event = visitor_poll_start_event();
        let other_public_key = URL_SAFE_NO_PAD.encode(
            ed25519_dalek::SigningKey::from_bytes(&[41u8; 32])
                .verifying_key()
                .to_bytes(),
        );
        event.sender = Some(virtual_sender(&other_public_key, "my-blog"));
        assert!(
            project(&event, Some(&poll_identity())).is_none(),
            "a valid proof sent by another virtual user must not be trusted"
        );
    }

    #[test]
    fn visitor_poll_start_without_server_name_is_rejected() {
        let event = visitor_poll_start_event();
        assert!(
            project_with_server(&event, Some(&poll_identity()), None).is_none(),
            "without a configured server name the sender cannot be bound"
        );
    }

    #[test]
    fn wire_tampering_with_unchanged_provenance_is_rejected() {
        // Each mutation changes the actual Poll wire content while leaving the
        // signed provenance untouched; trust must fail.
        let cases: [(&str, Tamper); 8] = [
            ("question", tamper_question),
            ("answer id", tamper_answer_id),
            ("answer text", tamper_answer_text),
            ("answer order", tamper_answer_order),
            ("kind", tamper_kind),
            ("custom kind", tamper_custom_kind),
            ("max_selections", tamper_max_selections),
            ("thread relation", tamper_thread_relation),
        ];

        for (name, mutate) in cases {
            let mut event = visitor_poll_start_event();
            mutate(&mut event);
            assert!(
                project(&event, Some(&poll_identity())).is_none(),
                "tampered wire {name} must not be trusted"
            );
        }
    }

    #[test]
    fn provenance_tampering_with_unchanged_wire_is_rejected() {
        // Each mutation changes the signed provenance while leaving the wire
        // Poll untouched; trust must fail on the signature or consistency.
        let cases: [(&str, Tamper); 4] = [
            ("content", tamper_provenance_content),
            ("operation_id", tamper_operation_id),
            ("challenge", tamper_challenge),
            ("signature", tamper_signature),
        ];

        for (name, mutate) in cases {
            let mut event = visitor_poll_start_event();
            mutate(&mut event);
            assert!(
                project(&event, Some(&poll_identity())).is_none(),
                "tampered provenance {name} must not be trusted"
            );
        }
    }

    #[test]
    fn matrix_native_poll_start_needs_no_cumments_proof() {
        // A non-virtual sender is governed by its Matrix identity, not the
        // frozen Cumments signature, so it projects without provenance.
        let mut source = visitor_poll_start_event();
        let content = source.content.take().unwrap();
        let mut event = direct_event(POLL_START_EVENT_TYPE, content);
        event.sender = Some("@alice:hs".to_string());
        event.content.as_mut().unwrap()["host.curious.cumments"] = serde_json::json!({});
        let parsed = project(&event, Some(&poll_identity())).expect("native poll projects");
        assert!(!parsed.is_virtual_user_sender);
        assert!(parsed.author_public_key.is_none());
    }
}

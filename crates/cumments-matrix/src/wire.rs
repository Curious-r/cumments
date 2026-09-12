//! Pure wire-format builders and helpers for Cumments Matrix events.
//!
//! Everything in this module is transport-agnostic: no HTTP, no driver state.
//! Keeping the wire format separate from `appservice.rs` makes it reviewable
//! and testable on its own.

use cumments_core::models::{CommentMedia, MediaKind};
use cumments_core::protocol::{
    MESSAGE_CONTENT_KEY, MESSAGE_SCHEMA_VERSION, METADATA_SCHEMA_VERSION, PROVENANCE_CONTENT_KEY,
    PROVENANCE_SCHEMA_VERSION,
};

/// Preferred alias localpart prefix. The Matrix spec recommends exclusive
/// user and alias namespaces begin with `_` after the sigil, e.g.
/// `@_irc_.*` / `#_irc_.*`.
pub(crate) const ALIAS_PREFIX: &str = "_cumments_";
pub(crate) fn site_space_alias_localpart(site_id: &str) -> String {
    format!("{}{}", ALIAS_PREFIX, site_id)
}

pub(crate) fn comment_room_alias_localpart(site_id: &str, page_slug: &str) -> String {
    format!("{}{}_{}", ALIAS_PREFIX, site_id, page_slug)
}

pub(crate) fn site_space_alias(server_name: &str, site_id: &str) -> String {
    format!("#{}:{}", site_space_alias_localpart(site_id), server_name)
}

pub(crate) fn comment_room_alias(server_name: &str, site_id: &str, page_slug: &str) -> String {
    format!(
        "#{}:{}",
        comment_room_alias_localpart(site_id, page_slug),
        server_name
    )
}
/// Whether a metadata state payload matches the expected Cumments identity.
/// Spaces carry `page_slug: null`; comment rooms carry the page slug.
pub(crate) fn metadata_matches(
    meta: &serde_json::Value,
    site_id: &str,
    page_slug: Option<&str>,
) -> bool {
    // v1 break: missing or non-1 → unsupported (fail closed).
    match meta.get("schema").and_then(|v| v.as_i64()) {
        Some(s) if s == METADATA_SCHEMA_VERSION => {}
        _ => return false,
    }
    let site_ok = meta.get("site_id").and_then(|v| v.as_str()) == Some(site_id);
    let slug_ok = match page_slug {
        Some(slug) => meta.get("page_slug").and_then(|v| v.as_str()) == Some(slug),
        None => matches!(meta.get("page_slug"), None | Some(serde_json::Value::Null)),
    };
    site_ok && slug_ok
}
/// Whether a room version still requires the room creator to be listed in
/// `m.room.power_levels.users`.
///
/// Room versions 1-11 explicitly privilege the creator through the power
/// levels event (and allow that power to be changed). From version 12 onward
/// the creator is never listed in the event and instead holds immutable
/// infinite power. Unknown/absent versions are treated conservatively as
/// explicit (the pre-v12 behaviour).
pub(crate) fn room_requires_explicit_creator(version: Option<&str>) -> bool {
    let major = version
        .and_then(|v| v.split(['.', '-']).next())
        .and_then(|v| v.parse::<u32>().ok());
    major.is_none_or(|v| v < 12)
}

/// Whether `user_id` is a room creator under the v12+ implicit-power rules.
///
/// Room versions 12+ define the creators as the `sender` of the
/// `m.room.create` event plus any `additional_creators` in its content;
/// either grants infinite power without an entry in
/// `m.room.power_levels.users`.
pub(crate) fn is_implicit_creator(
    room_version: Option<&str>,
    create_sender: &str,
    additional_creators: Option<&[String]>,
    user_id: &str,
) -> bool {
    !room_requires_explicit_creator(room_version)
        && (create_sender == user_id
            || additional_creators
                .is_some_and(|creators| creators.iter().any(|creator| creator.as_str() == user_id)))
}

/// Whether `sender_user_id` meets the room's `redact` threshold. Redactions
/// of other users' events require this power, so adopted rooms must satisfy
/// it or every Cumments delete submission will fail.
pub(crate) fn has_redact_power(power_levels: &serde_json::Value, sender_user_id: &str) -> bool {
    let user_power = power_levels
        .get("users")
        .and_then(|u| u.get(sender_user_id))
        .and_then(|v| v.as_i64())
        .or_else(|| power_levels.get("users_default").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    let required = power_levels
        .get("redact")
        .and_then(|v| v.as_i64())
        .unwrap_or(50);
    user_power >= required
}

/// Build the rich-reply fallback body following the pre-v1.13 spec format:
/// each line of the original body is prefixed with `> `, the first line also
/// carries the original sender's MXID, followed by a blank line and the reply
/// content. Returns `None` when there is nothing useful to quote.
pub(crate) fn reply_fallback_body(
    sender_mxid: &str,
    original: &str,
    content: &str,
) -> Option<String> {
    if original.is_empty() {
        return None;
    }
    let mut quoted = original
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("> <{}> {}", sender_mxid, line)
            } else {
                format!("> {}", line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    quoted.push_str("\n\n");
    quoted.push_str(content);
    Some(quoted)
}

/// Build the `m.room.message` content for a new Cumments comment.
#[allow(clippy::too_many_arguments)] // wire-format builders carry the full event payload
pub fn build_message_body(
    content: &str,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    submission_id: Option<i64>,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
    reply_to_body: Option<&str>,
    reply_to_sender: Option<&str>,
) -> serde_json::Value {
    // The body is the pure comment text unless a rich-reply fallback is
    // available: the quoted original is only for clients without relation
    // support. The structured block always carries the pure content.
    let body = match (reply_to, reply_to_body, reply_to_sender) {
        (Some(_), Some(original), Some(sender)) => {
            reply_fallback_body(sender, original, content).unwrap_or_else(|| content.to_string())
        }
        _ => content.to_string(),
    };
    let mut message_body = serde_json::json!({
        "msgtype": "m.text",
        "body": body,
    });
    message_body[MESSAGE_CONTENT_KEY] = serde_json::json!({
        "schema": MESSAGE_SCHEMA_VERSION,
        "public_key": author_public_key,
        "signature": author_signature,
        "challenge": author_challenge,
        // Structured content so the projector can store the pure body
        // instead of parsing it back out of the rich-reply fallback.
        // Display data (display name, avatar) is profile state and is
        // deliberately not part of the proof block.
        "content": content,
        "submission_id": submission_id,
    });
    match (reply_to, thread_root) {
        (Some(reply), Some(thread)) => {
            message_body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": thread,
                "m.in_reply_to": { "event_id": reply },
            });
        }
        (Some(reply), None) => {
            message_body["m.relates_to"] = serde_json::json!({
                "m.in_reply_to": { "event_id": reply },
            });
        }
        (None, Some(thread)) => {
            message_body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": thread,
            });
        }
        (None, None) => {}
    }
    message_body
}
/// Build the `m.room.message` content for a visitor media message
/// (image/audio/video/file). The structured block carries the media URL as
/// the canonical signed content.
pub fn build_media_body(
    media: &CommentMedia,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    submission_id: Option<i64>,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
) -> serde_json::Value {
    let msgtype = match media.kind {
        Some(MediaKind::Sticker) => "m.sticker",
        Some(MediaKind::Image) => "m.image",
        Some(MediaKind::Video) => "m.video",
        Some(MediaKind::Audio) => "m.audio",
        Some(MediaKind::File) => "m.file",
        None => match media.mimetype.as_deref() {
            Some(mime) if mime.starts_with("image/") => "m.image",
            Some(mime) if mime.starts_with("video/") => "m.video",
            Some(mime) if mime.starts_with("audio/") => "m.audio",
            _ => "m.file",
        },
    };
    let fallback = media
        .filename
        .clone()
        .unwrap_or_else(|| "media".to_string());
    let mut message_body = serde_json::json!({
        "msgtype": msgtype,
        "body": fallback,
        "url": media.url,
    });
    if let Some(filename) = &media.filename {
        message_body["filename"] = serde_json::json!(filename);
    }
    let mut info = serde_json::Map::new();
    if let Some(mimetype) = &media.mimetype {
        info.insert("mimetype".to_string(), serde_json::json!(mimetype));
    }
    if let Some(size) = media.size {
        info.insert("size".to_string(), serde_json::json!(size));
    }
    if let Some(width) = media.width {
        info.insert("w".to_string(), serde_json::json!(width));
    }
    if let Some(height) = media.height {
        info.insert("h".to_string(), serde_json::json!(height));
    }
    if !info.is_empty() {
        message_body["info"] = serde_json::Value::Object(info);
    }
    if media.voice {
        message_body["org.matrix.msc3245.voice"] = serde_json::json!({});
    }
    message_body[MESSAGE_CONTENT_KEY] = serde_json::json!({
        "schema": MESSAGE_SCHEMA_VERSION,
        "public_key": author_public_key,
        "signature": author_signature,
        "challenge": author_challenge,
        "content": media.url,
        "submission_id": submission_id,
    });
    match (reply_to, thread_root) {
        (Some(reply), Some(thread)) => {
            message_body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": thread,
                "m.in_reply_to": { "event_id": reply },
            });
        }
        (Some(reply), None) => {
            message_body["m.relates_to"] = serde_json::json!({
                "m.in_reply_to": { "event_id": reply },
            });
        }
        (None, Some(thread)) => {
            message_body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": thread,
            });
        }
        (None, None) => {}
    }
    message_body
}
/// Build the `m.reaction` content for a visitor reaction.
pub(crate) fn build_reaction_body(
    key: &str,
    target_event_id: &str,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
) -> serde_json::Value {
    serde_json::json!({
        "m.relates_to": {
            "rel_type": "m.annotation",
            "event_id": target_event_id,
            "key": key,
        },
        MESSAGE_CONTENT_KEY: {
            "schema": MESSAGE_SCHEMA_VERSION,
            "public_key": author_public_key,
            "signature": author_signature,
            "challenge": author_challenge,
            "content": key,
        }
    })
}

/// Build the content for a direct Cumments `org.matrix.msc3381.poll.response`
/// event (the pinned MSC3381 revision, not the remote `m.room.message`
/// wrapper).
///
/// The selections are emitted verbatim as the wire `answers` array — the
/// caller passes the canonical (deduplicated, byte-wise sorted) set, and the
/// same canonical order appears on the wire. The provenance block carries the
/// operation id and the exact canonical Vote semantic operation.
#[allow(clippy::too_many_arguments)] // wire-format builders carry the full event payload
pub fn build_poll_response_body(
    poll_event_id: &str,
    option_ids: &[String],
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    operation_id: &str,
    semantic_operation: &serde_json::Value,
) -> serde_json::Value {
    use crate::poll::PollResponseContent;

    let content = PollResponseContent::new(poll_event_id, option_ids.to_vec());
    let mut body = serde_json::to_value(&content).expect("poll response content serializes");
    body[PROVENANCE_CONTENT_KEY] = serde_json::json!({
        "schema": PROVENANCE_SCHEMA_VERSION,
        "operation_id": operation_id,
        "public_key": author_public_key,
        "signature": author_signature,
        "challenge": author_challenge,
        "content": semantic_operation,
    });
    body
}

/// Build the content for a direct Cumments `org.matrix.msc3381.poll.end`
/// event (the pinned MSC3381 revision, not the remote `m.room.message`
/// wrapper).
///
/// The end block and fallback text are produced according to MSC3381; the
/// Cumments provenance block carries the operation id and the exact canonical
/// END_POLL semantic operation.
#[allow(clippy::too_many_arguments)]
pub fn build_poll_end_body(
    poll_event_id: &str,
    fallback_text: &str,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    operation_id: &str,
    semantic_operation: &serde_json::Value,
) -> serde_json::Value {
    use crate::poll::PollEndContent;

    let content = PollEndContent::new(poll_event_id, fallback_text);
    let mut body = serde_json::to_value(&content).expect("poll end content serializes");
    body[PROVENANCE_CONTENT_KEY] = serde_json::json!({
        "schema": PROVENANCE_SCHEMA_VERSION,
        "operation_id": operation_id,
        "public_key": author_public_key,
        "signature": author_signature,
        "challenge": author_challenge,
        "content": semantic_operation,
    });
    body
}

/// Build the content for a direct Cumments `org.matrix.msc3381.poll.start`
/// event (the pinned MSC3381 revision, not the remote `m.room.message`
/// wrapper).
///
/// The poll block is produced by the typed poll wire layer; the Cumments
/// provenance block carries the durable `operation_id` and the exact
/// canonical semantic operation, and the optional thread/reply relation is
/// encoded like every other Cumments event.
#[allow(clippy::too_many_arguments)] // wire-format builders carry the full event payload
pub fn build_poll_start_body(
    question: &str,
    answers: &[cumments_core::poll::PollSemanticAnswer],
    kind: cumments_core::poll::PollSemanticKind,
    max_selections: u64,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    operation_id: &str,
    semantic_operation: &serde_json::Value,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
) -> serde_json::Value {
    use crate::poll::{PollAnswer, PollKind, PollStartContent};

    // Fallback body for clients without poll support (required by MSC3381 for
    // poll start events).
    let mut fallback = String::new();
    fallback.push_str(question);
    fallback.push_str("\n\n");
    for (index, answer) in answers.iter().enumerate() {
        fallback.push_str(&format!("{}. {}\n", index + 1, answer.text));
    }
    let fallback = fallback.trim_end().to_string();

    let poll_kind = match kind {
        cumments_core::poll::PollSemanticKind::Disclosed => PollKind::Disclosed,
        cumments_core::poll::PollSemanticKind::Undisclosed => PollKind::Undisclosed,
    };
    let content = PollStartContent::new(
        fallback,
        question,
        answers
            .iter()
            .map(|answer| PollAnswer::new(answer.id.clone(), answer.text.clone()))
            .collect(),
        poll_kind,
        max_selections,
    );
    let mut body = serde_json::to_value(&content).expect("poll start content serializes");

    body[PROVENANCE_CONTENT_KEY] = serde_json::json!({
        "schema": PROVENANCE_SCHEMA_VERSION,
        "operation_id": operation_id,
        "public_key": author_public_key,
        "signature": author_signature,
        "challenge": author_challenge,
        "content": semantic_operation,
    });
    apply_relations(&mut body, reply_to, thread_root);
    body
}

/// Attach the standard rich-reply / thread relation to an event content.
fn apply_relations(
    body: &mut serde_json::Value,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
) {
    match (reply_to, thread_root) {
        (Some(reply), Some(thread)) => {
            body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": thread,
                "m.in_reply_to": { "event_id": reply },
            });
        }
        (Some(reply), None) => {
            body["m.relates_to"] = serde_json::json!({
                "m.in_reply_to": { "event_id": reply },
            });
        }
        (None, Some(thread)) => {
            body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": thread,
            });
        }
        (None, None) => {}
    }
}

/// Build the `m.room.message` content for a visitor location (MSC3488).
#[allow(clippy::too_many_arguments)] // wire-format builders carry the full event payload
pub fn build_location_body(
    geo_uri: &str,
    description: Option<&str>,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    submission_id: Option<i64>,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "msgtype": "org.matrix.msc3488.location",
        "geo_uri": geo_uri,
        MESSAGE_CONTENT_KEY: {
            "schema": MESSAGE_SCHEMA_VERSION,
            "public_key": author_public_key,
            "signature": author_signature,
            "challenge": author_challenge,
            "content": geo_uri,
            "submission_id": submission_id,
        }
    });
    if let Some(description) = description {
        body["body"] = serde_json::json!(description);
    }
    match (reply_to, thread_root) {
        (Some(reply), Some(thread)) => {
            body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": thread,
                "m.in_reply_to": { "event_id": reply },
            });
        }
        (Some(reply), None) => {
            body["m.relates_to"] = serde_json::json!({
                "m.in_reply_to": { "event_id": reply },
            });
        }
        (None, Some(thread)) => {
            body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": thread,
            });
        }
        (None, None) => {}
    }
    body
}
/// Build the `m.room.message` content for an edit (`m.replace`).
pub(crate) fn build_edit_body(
    event_id: &str,
    new_content: &str,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    submission_id: Option<i64>,
) -> serde_json::Value {
    let mut new_content_obj = serde_json::json!({
        "msgtype": "m.text",
        "body": new_content,
    });
    new_content_obj[MESSAGE_CONTENT_KEY] = serde_json::json!({
        "schema": MESSAGE_SCHEMA_VERSION,
        "public_key": author_public_key,
        "signature": author_signature,
        "challenge": author_challenge,
        "content": new_content,
        "submission_id": submission_id,
    });
    serde_json::json!({
        "msgtype": "m.text",
        // Matrix edits require a fallback body starting with "* ".
        "body": format!("* {}", new_content),
        "m.new_content": new_content_obj,
        "m.relates_to": {
            "rel_type": "m.replace",
            "event_id": event_id,
        },
    })
}
/// Build the `m.room.redaction` content. When a delete proof is available it
/// is embedded as a JSON string in `reason` so the event log carries the
/// authorization independently of the submission queue.
pub(crate) fn build_redaction_body(proof: Option<&serde_json::Value>) -> serde_json::Value {
    match proof {
        Some(proof) => serde_json::json!({ "reason": proof.to_string() }),
        None => serde_json::json!({}),
    }
}

/// Percent-encode a string for safe use in URL path segments.
/// Matrix room IDs contain `!` and `:` — these are technically safe in
/// URL paths, but we encode them for correctness.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_matches_space() {
        let meta = json!({"schema": 1, "site_id": "my-blog", "page_slug": null});
        assert!(metadata_matches(&meta, "my-blog", None));
        assert!(!metadata_matches(&meta, "other", None));
        assert!(!metadata_matches(&meta, "my-blog", Some("hello")));
    }

    #[test]
    fn metadata_matches_space_without_slug_key() {
        // Missing schema is now unsupported (break), so legacy without schema fails
        let legacy = json!({"site_id": "my-blog"});
        assert!(!metadata_matches(&legacy, "my-blog", None));
        let with_schema = json!({"schema": 1, "site_id": "my-blog"});
        // Space without page_slug key but with schema is still Space (null handled)
        // This still fails because page_slug missing is treated as null, but schema present
        assert!(metadata_matches(&with_schema, "my-blog", None));
    }

    #[test]
    fn metadata_matches_comment_room() {
        let meta = json!({"schema": 1, "site_id": "my-blog", "page_slug": "hello-world"});
        assert!(metadata_matches(&meta, "my-blog", Some("hello-world")));
        assert!(!metadata_matches(&meta, "my-blog", None));
        assert!(!metadata_matches(&meta, "my-blog", Some("other")));
    }

    #[test]
    fn metadata_schema_legacy_rejected() {
        // Missing schema → unsupported under v1 break
        let legacy = json!({"site_id": "my-blog", "page_slug": "hello"});
        assert!(!metadata_matches(&legacy, "my-blog", Some("hello")));
        let legacy_space = json!({"site_id": "my-blog", "page_slug": null});
        assert!(!metadata_matches(&legacy_space, "my-blog", None));
    }

    #[test]
    fn metadata_schema_1_accepted_and_unknown_rejected() {
        let with_schema = json!({"schema": 1, "site_id": "my-blog", "page_slug": "hello"});
        assert!(metadata_matches(&with_schema, "my-blog", Some("hello")));
        // Unknown future schema must fail closed
        let unknown = json!({"schema": 2, "site_id": "my-blog", "page_slug": "hello"});
        assert!(!metadata_matches(&unknown, "my-blog", Some("hello")));
        let unknown_space = json!({"schema": 99, "site_id": "my-blog", "page_slug": null});
        assert!(!metadata_matches(&unknown_space, "my-blog", None));
        // Non-integer schema is unsupported
        let bad_type = json!({"schema": "1", "site_id": "my-blog", "page_slug": "hello"});
        assert!(!metadata_matches(&bad_type, "my-blog", Some("hello")));
    }

    #[test]
    fn metadata_schema_unknown_fields_tolerant_for_known_schema() {
        let with_extra = json!({"schema": 1, "site_id": "my-blog", "page_slug": "hello", "extra": "ignore-me", "owner": "something"});
        assert!(metadata_matches(&with_extra, "my-blog", Some("hello")));
        // Legacy without schema is now unsupported under v1 break, even with extra fields
        let legacy_extra = json!({"site_id": "my-blog", "page_slug": "hello", "extra": 123});
        assert!(!metadata_matches(&legacy_extra, "my-blog", Some("hello")));
    }

    #[test]
    fn redact_power_uses_explicit_or_default_threshold() {
        let power = json!({
            "users": { "@bot:example.com": 75 },
            "redact": 75
        });
        assert!(has_redact_power(&power, "@bot:example.com"));
        assert!(!has_redact_power(&power, "@other:example.com"));

        // Default redact level is 50.
        let default_power = json!({ "users": { "@bot:example.com": 50 } });
        assert!(has_redact_power(&default_power, "@bot:example.com"));
        assert!(!has_redact_power(&json!({}), "@bot:example.com"));
    }

    #[test]
    fn percent_encode_encodes_alias_hash_and_colon() {
        assert_eq!(
            percent_encode("#_cumments_my-blog:example.com"),
            "%23_cumments_my-blog%3Aexample.com"
        );
        assert_eq!(percent_encode("!abc:example.com"), "%21abc%3Aexample.com");
    }

    #[test]
    fn alias_helpers_build_underscored_aliases() {
        assert_eq!(
            site_space_alias("example.com", "my-blog"),
            "#_cumments_my-blog:example.com"
        );
        assert_eq!(
            comment_room_alias("example.com", "my-blog", "hello-world"),
            "#_cumments_my-blog_hello-world:example.com"
        );
    }

    #[test]
    fn message_body_uses_namespaced_content_block() {
        let body = build_message_body(
            "hello <b>",
            "pubkey",
            "sig",
            "chal",
            Some(7),
            None,
            None,
            None,
            None,
        );

        assert_eq!(body["msgtype"].as_str(), Some("m.text"));
        assert_eq!(body["body"].as_str(), Some("hello <b>"));
        assert!(body.get("format").is_none());
        assert!(body.get("formatted_body").is_none());

        let ns = body.get(MESSAGE_CONTENT_KEY).expect("namespaced block");
        assert_eq!(ns["public_key"].as_str(), Some("pubkey"));
        assert_eq!(ns["signature"].as_str(), Some("sig"));
        assert_eq!(ns["challenge"].as_str(), Some("chal"));
        assert_eq!(ns["content"].as_str(), Some("hello <b>"));
        assert_eq!(ns["submission_id"].as_i64(), Some(7));
        assert!(ns.get("visitor_id").is_none());
        assert!(ns.get("displayname").is_none());

        assert!(body.get("cumments_visitor_id").is_none());
        assert!(body.get("cumments_submission_id").is_none());
    }

    #[test]
    fn media_body_uses_msgtype_and_embed_url_as_content() {
        let body = build_media_body(
            &CommentMedia {
                kind: None,
                url: "mxc://hs/abc".to_string(),
                filename: Some("cat.png".to_string()),
                mimetype: Some("image/png".to_string()),
                size: Some(1024),
                width: Some(100),
                height: Some(80),
                voice: false,
            },
            "pubkey",
            "sig",
            "chal",
            Some(7),
            None,
            None,
        );
        assert_eq!(body["msgtype"], "m.image");
        assert_eq!(body["body"], "cat.png");
        assert_eq!(body["url"], "mxc://hs/abc");
        assert_eq!(body["info"]["mimetype"], "image/png");
        assert_eq!(body["info"]["w"], 100);
        assert_eq!(body[MESSAGE_CONTENT_KEY]["content"], "mxc://hs/abc");
        assert_eq!(body[MESSAGE_CONTENT_KEY]["submission_id"], 7);
        assert!(body.get("org.matrix.msc3245.voice").is_none());
    }

    #[test]
    fn voice_media_uses_audio_msgtype_and_voice_flag() {
        let body = build_media_body(
            &CommentMedia {
                kind: None,
                url: "mxc://hs/voice".to_string(),
                filename: None,
                mimetype: Some("audio/webm".to_string()),
                size: None,
                width: None,
                height: None,
                voice: true,
            },
            "pubkey",
            "sig",
            "chal",
            None,
            None,
            None,
        );
        assert_eq!(body["msgtype"], "m.audio");
        assert!(body.get("org.matrix.msc3245.voice").is_some());
    }

    #[test]
    fn reaction_body_carries_annotation_and_proof() {
        let body = build_reaction_body("👍", "$target:hs", "pk", "sig", "chal");
        assert_eq!(body["m.relates_to"]["rel_type"], "m.annotation");
        assert_eq!(body["m.relates_to"]["event_id"], "$target:hs");
        assert_eq!(body["m.relates_to"]["key"], "👍");
        assert_eq!(body[MESSAGE_CONTENT_KEY]["content"], "👍");
        assert!(body[MESSAGE_CONTENT_KEY].get("visitor_id").is_none());
        assert!(body[MESSAGE_CONTENT_KEY].get("displayname").is_none());
    }

    #[test]
    fn poll_start_body_uses_direct_event_type_and_provenance() {
        use cumments_core::poll::{PollSemanticAnswer, PollSemanticKind};
        let answers = vec![
            PollSemanticAnswer::new("slot-10am", "10:00 AM UTC"),
            PollSemanticAnswer::new("slot-2pm", "2:00 PM UTC"),
        ];
        let semantic =
            serde_json::json!(["POLL", ["s", "p", null, null], ["q", [], "disclosed", 1], 1]);
        let body = build_poll_start_body(
            "Which?",
            &answers,
            PollSemanticKind::Disclosed,
            1,
            "pk",
            "sig",
            "chal",
            "op-1",
            &semantic,
            None,
            None,
        );
        // No `m.room.message` wrapper and no `msgtype`.
        assert!(body.get("msgtype").is_none());
        assert!(!body.to_string().contains("m.room.message"));
        assert_eq!(
            body["org.matrix.msc3381.poll.start"]["question"]["org.matrix.msc1767.text"],
            "Which?"
        );
        assert_eq!(
            body["org.matrix.msc3381.poll.start"]["kind"],
            "org.matrix.msc3381.poll.disclosed"
        );
        assert_eq!(body["org.matrix.msc3381.poll.start"]["max_selections"], 1);
        let wire_answers = body["org.matrix.msc3381.poll.start"]["answers"]
            .as_array()
            .expect("answers");
        assert_eq!(wire_answers.len(), 2);
        assert_eq!(wire_answers[0]["id"], "slot-10am");
        assert_eq!(wire_answers[1]["id"], "slot-2pm");
        assert_eq!(wire_answers[0]["org.matrix.msc1767.text"], "10:00 AM UTC");
        // Required single-string fallback.
        let fallback = body["org.matrix.msc1767.text"].as_str().expect("fallback");
        assert!(fallback.contains("Which?"));
        assert!(fallback.contains("1. 10:00 AM UTC"));
        // Provenance block carries the operation id and canonical operation.
        let provenance = &body[PROVENANCE_CONTENT_KEY];
        assert_eq!(provenance["schema"], PROVENANCE_SCHEMA_VERSION);
        assert_eq!(provenance["operation_id"], "op-1");
        assert_eq!(provenance["public_key"], "pk");
        assert_eq!(provenance["signature"], "sig");
        assert_eq!(provenance["challenge"], "chal");
        assert_eq!(provenance["content"], semantic);
    }

    #[test]
    fn poll_start_body_supports_reply_and_thread_relations() {
        use cumments_core::poll::{PollSemanticAnswer, PollSemanticKind};
        let answers = vec![
            PollSemanticAnswer::new("a", "A"),
            PollSemanticAnswer::new("b", "B"),
        ];
        let semantic = serde_json::json!(["POLL"]);
        let build = |reply_to: Option<&str>, thread_root: Option<&str>| {
            build_poll_start_body(
                "q?",
                &answers,
                PollSemanticKind::Undisclosed,
                1,
                "pk",
                "sig",
                "chal",
                "op",
                &semantic,
                reply_to,
                thread_root,
            )
        };
        let with_reply = build(Some("$p:hs"), None);
        assert_eq!(
            with_reply["m.relates_to"]["m.in_reply_to"]["event_id"],
            "$p:hs"
        );
        let with_thread = build(None, Some("$t:hs"));
        assert_eq!(with_thread["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(with_thread["m.relates_to"]["event_id"], "$t:hs");
        let both = build(Some("$p:hs"), Some("$t:hs"));
        assert_eq!(both["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(both["m.relates_to"]["m.in_reply_to"]["event_id"], "$p:hs");
    }

    #[test]
    fn poll_response_body_uses_direct_event_type_and_provenance() {
        let answers = vec!["a".to_string(), "b".to_string()];
        let semantic = serde_json::json!(["VOTE", ["s", "p", "$poll:hs"], ["a", "b"], 1]);
        let body =
            build_poll_response_body("$poll:hs", &answers, "pk", "sig", "chal", "op-1", &semantic);
        // No `m.room.message` wrapper and no `msgtype`.
        assert!(body.get("msgtype").is_none());
        assert!(!body.to_string().contains("m.room.message"));
        assert_eq!(body["m.relates_to"]["rel_type"], "m.reference");
        assert_eq!(body["m.relates_to"]["event_id"], "$poll:hs");
        assert_eq!(
            body["org.matrix.msc3381.poll.response"]["answers"],
            serde_json::json!(["a", "b"])
        );
        let provenance = &body[PROVENANCE_CONTENT_KEY];
        assert_eq!(provenance["schema"], PROVENANCE_SCHEMA_VERSION);
        assert_eq!(provenance["operation_id"], "op-1");
        assert_eq!(provenance["public_key"], "pk");
        assert_eq!(provenance["signature"], "sig");
        assert_eq!(provenance["challenge"], "chal");
        assert_eq!(provenance["content"], semantic);
    }

    #[test]
    fn location_body_carries_geo_uri_and_proof() {
        let body = build_location_body(
            "geo:31.2,121.5",
            Some("here"),
            "pk",
            "sig",
            "chal",
            Some(9),
            None,
            None,
        );
        assert_eq!(body["msgtype"], "org.matrix.msc3488.location");
        assert_eq!(body["geo_uri"], "geo:31.2,121.5");
        assert_eq!(body["body"], "here");
        assert_eq!(body[MESSAGE_CONTENT_KEY]["content"], "geo:31.2,121.5");
        assert_eq!(body[MESSAGE_CONTENT_KEY]["submission_id"], 9);
        assert!(body[MESSAGE_CONTENT_KEY].get("displayname").is_none());
    }

    #[test]
    fn message_body_with_reply_uses_standard_relation() {
        let body = build_message_body(
            "hello",
            "pubkey",
            "sig",
            "chal",
            Some(7),
            Some("$parent:hs"),
            None,
            Some("original line one\noriginal line two"),
            Some("@alice:hs"),
        );

        assert_eq!(
            body["body"].as_str(),
            Some("> <@alice:hs> original line one\n> original line two\n\nhello")
        );
        // The structured block still carries only the pure reply content.
        assert_eq!(body[MESSAGE_CONTENT_KEY]["content"].as_str(), Some("hello"));
        assert_eq!(
            body["m.relates_to"]["m.in_reply_to"]["event_id"].as_str(),
            Some("$parent:hs")
        );
        assert!(body["m.relates_to"].get("rel_type").is_none());
        assert!(body.get(MESSAGE_CONTENT_KEY).is_some());
    }

    #[test]
    fn reply_without_known_original_keeps_pure_body() {
        let body = build_message_body(
            "hello",
            "pubkey",
            "sig",
            "chal",
            Some(7),
            Some("$parent:hs"),
            None,
            None,
            None,
        );
        assert_eq!(body["body"].as_str(), Some("hello"));
        assert_eq!(
            body["m.relates_to"]["m.in_reply_to"]["event_id"].as_str(),
            Some("$parent:hs")
        );
    }

    #[test]
    fn thread_and_reply_can_coexist() {
        let body = build_message_body(
            "hello",
            "pubkey",
            "sig",
            "chal",
            None,
            Some("$parent:hs"),
            Some("$thread:hs"),
            None,
            None,
        );
        assert_eq!(body["m.relates_to"]["rel_type"].as_str(), Some("m.thread"));
        assert_eq!(
            body["m.relates_to"]["event_id"].as_str(),
            Some("$thread:hs")
        );
        assert_eq!(
            body["m.relates_to"]["m.in_reply_to"]["event_id"].as_str(),
            Some("$parent:hs")
        );
    }

    #[test]
    fn reply_fallback_quotes_multiline_original() {
        assert_eq!(
            reply_fallback_body("@alice:hs", "first\nsecond", "reply").as_deref(),
            Some("> <@alice:hs> first\n> second\n\nreply")
        );
        assert_eq!(reply_fallback_body("@alice:hs", "", "reply"), None);
    }

    #[test]
    fn thread_only_message_encodes_membership_without_reply() {
        // Thread membership without a direct reply is a first-class creation
        // state: the event expresses m.thread only, with no m.in_reply_to
        // and no fallback quote in the body.
        let body = build_message_body(
            "hello",
            "pubkey",
            "sig",
            "chal",
            Some(7),
            None,
            Some("$thread:hs"),
            None,
            None,
        );
        assert_eq!(body["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(body["m.relates_to"]["event_id"], "$thread:hs");
        assert!(body["m.relates_to"].get("m.in_reply_to").is_none());
        assert_eq!(body["body"], "hello");
        assert!(
            body["m.relates_to"].get("is_falling_back").is_none(),
            "the sender never emits is_falling_back; it is a receiver-side interpretation"
        );
    }

    #[test]
    fn media_body_supports_reply_and_thread_relations() {
        let media = CommentMedia {
            kind: None,
            url: "mxc://hs/abc".to_string(),
            filename: Some("cat.png".to_string()),
            mimetype: None,
            size: None,
            width: None,
            height: None,
            voice: false,
        };
        let with_reply = build_media_body(&media, "pk", "sig", "chal", None, Some("$p:hs"), None);
        assert_eq!(
            with_reply["m.relates_to"]["m.in_reply_to"]["event_id"],
            "$p:hs"
        );
        assert!(with_reply["m.relates_to"].get("rel_type").is_none());

        let with_thread = build_media_body(&media, "pk", "sig", "chal", None, None, Some("$t:hs"));
        assert_eq!(with_thread["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(with_thread["m.relates_to"]["event_id"], "$t:hs");
        assert!(with_thread["m.relates_to"].get("m.in_reply_to").is_none());

        let both = build_media_body(
            &media,
            "pk",
            "sig",
            "chal",
            None,
            Some("$p:hs"),
            Some("$t:hs"),
        );
        assert_eq!(both["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(both["m.relates_to"]["event_id"], "$t:hs");
        assert_eq!(both["m.relates_to"]["m.in_reply_to"]["event_id"], "$p:hs");
    }

    #[test]
    fn location_body_supports_reply_and_thread_relations() {
        let with_reply = build_location_body(
            "geo:31.2,121.5",
            None,
            "pk",
            "sig",
            "chal",
            None,
            Some("$p:hs"),
            None,
        );
        assert_eq!(
            with_reply["m.relates_to"]["m.in_reply_to"]["event_id"],
            "$p:hs"
        );
        assert!(with_reply["m.relates_to"].get("rel_type").is_none());

        let with_thread = build_location_body(
            "geo:31.2,121.5",
            None,
            "pk",
            "sig",
            "chal",
            None,
            None,
            Some("$t:hs"),
        );
        assert_eq!(with_thread["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(with_thread["m.relates_to"]["event_id"], "$t:hs");
        assert!(with_thread["m.relates_to"].get("m.in_reply_to").is_none());

        let both = build_location_body(
            "geo:31.2,121.5",
            None,
            "pk",
            "sig",
            "chal",
            None,
            Some("$p:hs"),
            Some("$t:hs"),
        );
        assert_eq!(both["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(both["m.relates_to"]["event_id"], "$t:hs");
        assert_eq!(both["m.relates_to"]["m.in_reply_to"]["event_id"], "$p:hs");
    }

    #[test]
    fn edit_body_uses_namespaced_block_in_new_content() {
        let body = build_edit_body(
            "$original:hs",
            "edited <b>",
            "pubkey",
            "sig",
            "chal",
            Some(42),
        );

        assert_eq!(body["msgtype"].as_str(), Some("m.text"));
        assert_eq!(body["body"].as_str(), Some("* edited <b>"));
        assert_eq!(body["m.relates_to"]["rel_type"].as_str(), Some("m.replace"));
        assert_eq!(
            body["m.relates_to"]["event_id"].as_str(),
            Some("$original:hs")
        );

        let new_content = body.get("m.new_content").expect("new content");
        assert_eq!(new_content["body"].as_str(), Some("edited <b>"));
        assert!(new_content.get("format").is_none());
        assert!(new_content.get("formatted_body").is_none());
        let ns = new_content
            .get(MESSAGE_CONTENT_KEY)
            .expect("namespaced block");
        assert_eq!(ns["public_key"].as_str(), Some("pubkey"));
        assert_eq!(ns["signature"].as_str(), Some("sig"));
        assert_eq!(ns["challenge"].as_str(), Some("chal"));
        assert_eq!(ns["content"].as_str(), Some("edited <b>"));
        assert_eq!(ns["submission_id"].as_i64(), Some(42));
        assert!(ns.get("visitor_id").is_none());
        assert!(ns.get("displayname").is_none());

        assert!(body.get(MESSAGE_CONTENT_KEY).is_none());
        assert!(body.get("cumments_submission_id").is_none());
    }

    #[test]
    fn redaction_body_embeds_proof_as_reason() {
        let proof = json!({
            "host.curious.cumments.redaction": {
                "public_key": "pk",
                "signature": "sig",
                "challenge": "chal",
            }
        });
        let body = build_redaction_body(Some(&proof));
        assert_eq!(body["reason"], proof.to_string());
        assert_eq!(build_redaction_body(None), json!({}));
    }

    #[test]
    fn explicit_creator_required_only_below_room_version_12() {
        assert!(room_requires_explicit_creator(None));
        assert!(room_requires_explicit_creator(Some("1")));
        assert!(room_requires_explicit_creator(Some("11")));
        assert!(room_requires_explicit_creator(Some("1.2")));
        assert!(!room_requires_explicit_creator(Some("12")));
        assert!(!room_requires_explicit_creator(Some("13")));
        assert!(!room_requires_explicit_creator(Some("12.1")));
    }

    #[test]
    fn implicit_creator_uses_sender_or_additional_creators_only_for_v12_plus() {
        let bot = "@_cumments_bot:example.com";
        let other = "@other:example.com";
        let additional: Vec<String> = vec![bot.to_string()];

        // Pre-v12 rooms privilege the creator through the power levels
        // event, so the create event's sender alone does not imply power.
        assert!(!is_implicit_creator(Some("11"), bot, None, bot));
        assert!(!is_implicit_creator(None, bot, None, bot));
        assert!(!is_implicit_creator(
            Some("11"),
            other,
            Some(&additional),
            bot
        ));

        // v12+ defines the sender (and additional_creators) as creators.
        assert!(is_implicit_creator(Some("12"), bot, None, bot));
        assert!(is_implicit_creator(Some("13"), bot, None, bot));
        assert!(is_implicit_creator(
            Some("12"),
            other,
            Some(&additional),
            bot
        ));
        assert!(!is_implicit_creator(Some("12"), other, None, bot));
        assert!(!is_implicit_creator(Some("12"), bot, None, other));
    }

    #[test]
    fn all_message_builders_emit_schema_1() {
        let text = build_message_body("hi", "pk", "sig", "chal", None, None, None, None, None);
        assert_eq!(text[MESSAGE_CONTENT_KEY]["schema"].as_i64(), Some(1));
        let media = build_media_body(
            &CommentMedia {
                kind: None,
                url: "mxc://hs/a".into(),
                filename: None,
                mimetype: None,
                size: None,
                width: None,
                height: None,
                voice: false,
            },
            "pk",
            "sig",
            "chal",
            None,
            None,
            None,
        );
        assert_eq!(media[MESSAGE_CONTENT_KEY]["schema"].as_i64(), Some(1));
        let reaction = build_reaction_body("👍", "$t:hs", "pk", "sig", "chal");
        assert_eq!(reaction[MESSAGE_CONTENT_KEY]["schema"].as_i64(), Some(1));
        // The direct poll response carries the Cumments provenance block.
        let vote = build_poll_response_body(
            "$p:hs",
            &["1".to_string()],
            "pk",
            "sig",
            "chal",
            "op",
            &json!(["VOTE"]),
        );
        assert_eq!(vote[PROVENANCE_CONTENT_KEY]["schema"].as_i64(), Some(1));
        assert!(vote.get(MESSAGE_CONTENT_KEY).is_none());
        // The direct poll start carries the Cumments provenance block instead
        // of the `m.room.message` block.
        let poll = build_poll_start_body(
            "q?",
            &[
                cumments_core::poll::PollSemanticAnswer::new("a", "A"),
                cumments_core::poll::PollSemanticAnswer::new("b", "B"),
            ],
            cumments_core::poll::PollSemanticKind::Disclosed,
            1,
            "pk",
            "sig",
            "chal",
            "op",
            &json!(["POLL"]),
            None,
            None,
        );
        assert_eq!(poll[PROVENANCE_CONTENT_KEY]["schema"].as_i64(), Some(1));
        assert!(poll.get(MESSAGE_CONTENT_KEY).is_none());
        // The direct poll end carries the Cumments provenance block.
        let end = build_poll_end_body(
            "$p:hs",
            "The poll has ended.",
            "pk",
            "sig",
            "chal",
            "op-end",
            &json!(["END_POLL"]),
        );
        assert_eq!(end[PROVENANCE_CONTENT_KEY]["schema"].as_i64(), Some(1));
        assert!(end.get(MESSAGE_CONTENT_KEY).is_none());
        assert_eq!(end["m.relates_to"]["event_id"].as_str(), Some("$p:hs"));
        assert_eq!(
            end["m.relates_to"]["rel_type"].as_str(),
            Some("m.reference")
        );
        assert_eq!(
            end["org.matrix.msc1767.text"].as_str(),
            Some("The poll has ended.")
        );
        assert!(end.get("org.matrix.msc3381.poll.end").is_some());

        let loc = build_location_body("geo:1,2", None, "pk", "sig", "chal", None, None, None);
        assert_eq!(loc[MESSAGE_CONTENT_KEY]["schema"].as_i64(), Some(1));

        let edit = build_edit_body("$o:hs", "new", "pk", "sig", "chal", None);
        assert_eq!(
            edit["m.new_content"][MESSAGE_CONTENT_KEY]["schema"].as_i64(),
            Some(1)
        );
        assert!(edit.get(MESSAGE_CONTENT_KEY).is_none());
    }

    #[test]
    fn message_builders_unknown_extra_fields_tolerant() {
        // Existing builders do not yet emit extra fields, but parsers must
        // ignore them. This test documents the contract: a payload with
        // schema 1 plus unknown extra keys still matches.
        let mut extra_meta = json!({
            "schema": 1,
            "site_id": "my-blog",
            "page_slug": "hello",
            "extra": "keep",
            "another": 123
        });
        assert!(metadata_matches(&extra_meta, "my-blog", Some("hello")));
        // Mutate to schema 2 → must reject even with extra fields
        extra_meta["schema"] = json!(2);
        assert!(!metadata_matches(&extra_meta, "my-blog", Some("hello")));
    }
}

//! Request and response DTOs for the Cumments API.

use crate::error::AppError;
use axum::http::{HeaderMap, HeaderName};
use cumments_core::models::{CommentMedia, Message};
use cumments_core::site_auth::sha256_hex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use validator::Validate;

/// The `Idempotency-Key` request header used by all async write submissions.
pub(crate) static IDEMPOTENCY_KEY_HEADER: LazyLock<HeaderName> =
    LazyLock::new(|| HeaderName::from_static("idempotency-key"));

/// Response header marking an idempotent replay.
pub(crate) static IDEMPOTENT_REPLAYED: LazyLock<HeaderName> =
    LazyLock::new(|| HeaderName::from_static("idempotent-replayed"));

/// Reads and validates the mandatory `Idempotency-Key` header.
///
/// Keys are 8-255 printable ASCII characters. Validation failures return a
/// 400 and never record the key, so the same key can be retried with a valid
/// request.
pub(crate) fn extract_idempotency_key(headers: &HeaderMap) -> Result<String, AppError> {
    let value = headers.get(&*IDEMPOTENCY_KEY_HEADER).ok_or_else(|| {
        AppError::IdempotencyKeyRequired(
            "Idempotency-Key header is required for write requests.".to_string(),
        )
    })?;
    let value = value.to_str().map_err(|_| {
        AppError::InvalidIdempotencyKey(
            "Idempotency-Key must contain only printable ASCII characters.".to_string(),
        )
    })?;
    if !(8..=255).contains(&value.len()) {
        return Err(AppError::InvalidIdempotencyKey(
            "Idempotency-Key must be 8-255 characters long.".to_string(),
        ));
    }
    if !value.bytes().all(|b| (0x21..=0x7E).contains(&b)) {
        return Err(AppError::InvalidIdempotencyKey(
            "Idempotency-Key must contain only printable ASCII characters ".to_string()
                + "(no spaces or control characters).",
        ));
    }
    Ok(value.to_owned())
}

/// Canonical fingerprint of one write request.
///
/// `METHOD\npath\nsha256(body)` — the body is hashed first so the fingerprint
/// stays compact for large payloads. The path is reconstructed from the
/// validated route parameters rather than the raw URL, so equivalent
/// percent-encoding choices still produce the same fingerprint.
pub(crate) fn request_fingerprint(method: &str, path: &str, body: &[u8]) -> String {
    format!("{}\n{}\n{}", method, path, sha256_hex(body))
}

/// The query parameters for pagination (sent as JSON body for QUERY method).
#[derive(Debug, Deserialize, Validate)]
pub struct PaginationQuery {
    // The upper bound keeps `(page - 1) * per_page` inside i64 even with the
    // largest allowed per_page (100).
    #[validate(range(min = 1, max = 1_000_000))]
    pub page: Option<i64>,
    #[validate(range(min = 1, max = 100))]
    pub per_page: Option<i64>,
    /// Optional personalization: when both are present and the signature
    /// verifies, each `ReactionSummary.mine` is set for the requesting
    /// visitor. This is a derived view, never stored, and does not change
    /// the anonymous default.
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: Option<String>,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: Option<String>,
}

#[derive(Serialize)]
pub struct PaginatedResponse {
    pub data: Vec<Message>,
    pub meta: PaginationMeta,
}

pub use cumments_core::models::PaginationMeta;

/// Request DTO for registering a site.
///
/// `site_id` is optional: without it the server generates an unguessable
/// random id; with it the caller picks the id used in Matrix aliases and the
/// Space display name. Chosen ids are first-come and must match the `site_id`
/// format (lowercase `[a-z0-9-]`, 1-64 characters).
#[derive(Debug, Default, Deserialize)]
pub struct RegisterSiteRequest {
    #[serde(default)]
    pub site_id: Option<String>,
}

/// The response for the `GET /api/challenge` endpoint.
#[derive(Serialize)]
pub struct ChallengeResponse {
    pub prefix: String,
    pub difficulty: u32,
}

fn validate_post_content(req: &PostCommentRequest) -> Result<(), validator::ValidationError> {
    // media present => content is just a filename fallback and may be empty;
    // otherwise the comment must carry visible text.
    if req.media.is_none() && req.content.trim().is_empty() {
        let mut err = validator::ValidationError::new("content_empty");
        err.message = Some("content must not be empty without a media attachment.".into());
        return Err(err);
    }
    Ok(())
}

/// Request DTO for posting a comment.
#[derive(Debug, Deserialize, Validate)]
#[validate(schema(function = "validate_post_content"))]
pub struct PostCommentRequest {
    #[validate(custom(function = "crate::validation::validate_comment_content"))]
    pub content: String,
    /// Optional media attachment; when present the signature covers
    /// `media.url` and `content` is only the fallback filename.
    #[serde(default)]
    pub media: Option<CommentMedia>,
    /// Display name to write to the virtual user's Matrix profile. It is
    /// presentation data and is deliberately not covered by the author
    /// signature; the signed payload covers only content and reply relation.
    #[validate(custom(function = "crate::validation::validate_display_name"))]
    pub display_name: String,
    /// Ed25519 public key of the author (base64url, 32 bytes raw).
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    /// Ed25519 signature over the canonical POST message.
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    /// Parent comment for a reply (`$event:hs`).
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Thread root (`$event:hs`). Orthogonal to `reply_to`.
    #[serde(default)]
    pub thread_root: Option<String>,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

/// Request DTO for deleting a comment. The body carries only the author
/// proof; the target is addressed by path.
#[derive(Debug, Deserialize, Validate)]
pub struct DeleteCommentRequest {
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

/// Request DTO for updating a comment.
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateCommentRequest {
    #[validate(custom(function = "crate::validation::validate_comment_content_update"))]
    pub content: String,
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

/// Request DTO for reacting to a comment.
#[derive(Debug, Deserialize, Validate)]
pub struct ReactRequest {
    #[validate(custom(function = "crate::validation::validate_reaction_key"))]
    pub key: String,
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

/// Request DTO for removing a reaction (key comes from path).
#[derive(Debug, Deserialize, Validate)]
pub struct UnreactRequest {
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

/// Request DTO for voting on a poll.
#[derive(Debug, Deserialize, Validate)]
pub struct VoteRequest {
    #[validate(length(min = 1, max = 128))]
    pub option_id: String,
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

fn default_poll_max_selections() -> u8 {
    1
}

/// Validate poll-specific constraints that `validator` cannot express
/// (whitespace, control characters, per-option length).
pub fn validate_poll_details(req: &PollRequest) -> Result<(), String> {
    if req.question.trim().is_empty() {
        return Err("question must not be empty or whitespace".to_string());
    }
    if req.question.trim() != req.question {
        return Err("question must not have leading or trailing whitespace".to_string());
    }
    if req.question.chars().any(|c| c.is_control()) {
        return Err("question must not contain control characters".to_string());
    }
    for option in &req.options {
        if option.trim().is_empty() {
            return Err("poll options must not be empty or whitespace".to_string());
        }
        if option.trim() != option {
            return Err("poll options must not have leading or trailing whitespace".to_string());
        }
        if option.chars().any(|c| c.is_control()) {
            return Err("poll option must not contain control characters".to_string());
        }
        if crate::validation::grapheme_len(option) > 200 {
            return Err("poll option must be at most 200 grapheme clusters".to_string());
        }
    }
    if req.max_selections != 1 {
        return Err("max_selections must be 1".to_string());
    }
    Ok(())
}

/// Request DTO for creating a poll (MSC3381).
///
/// The single-select restriction is enforced here: `max_selections` must be
/// exactly `1`. The wire format preserves MSC3381's `max_selections` so the
/// projector can faithfully store the declared limit, but the authoring API
/// remains single-select as documented in `docs/data-model.md`.
#[derive(Debug, Deserialize, Validate)]
pub struct PollRequest {
    #[validate(custom(function = "crate::validation::validate_poll_question"))]
    pub question: String,
    #[validate(length(min = 2, max = 20))]
    pub options: Vec<String>,
    #[serde(default = "default_poll_max_selections")]
    #[validate(range(min = 1, max = 1))]
    pub max_selections: u8,
    /// Display name to write to the virtual user's Matrix profile. It is
    /// presentation data and is deliberately not covered by the author
    /// signature; the signed payload covers only the poll payload.
    #[validate(custom(function = "crate::validation::validate_display_name"))]
    pub display_name: String,
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub thread_root: Option<String>,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

/// Request DTO for posting a location. Like `PostCommentRequest`, it may
/// carry `reply_to` / `thread_root` so locations can start or join threads.
#[derive(Debug, Deserialize, Validate)]
pub struct LocationRequest {
    #[validate(length(min = 4, max = 512))]
    pub geo_uri: String,
    #[validate(custom(function = "crate::validation::validate_location_description"))]
    #[serde(default)]
    pub description: Option<String>,
    /// Display name to write to the virtual user's Matrix profile. It is
    /// presentation data and is deliberately not covered by the author
    /// signature; the signed payload covers only the geo URI.
    #[validate(custom(function = "crate::validation::validate_display_name"))]
    pub display_name: String,
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub thread_root: Option<String>,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use validator::Validate;

    fn valid_poll() -> PollRequest {
        PollRequest {
            question: "Best?".to_string(),
            options: vec!["A".to_string(), "B".to_string()],
            max_selections: 1,
            display_name: "Alice".to_string(),
            author_public_key: "pk".to_string(),
            author_signature: "sig".to_string(),
            reply_to: None,
            thread_root: None,
            challenge_response: "chal|nonce".to_string(),
        }
    }

    #[test]
    fn poll_request_validates_success() {
        assert!(valid_poll().validate().is_ok());
    }

    #[test]
    fn poll_question_empty_rejected() {
        let mut req = valid_poll();
        req.question = "".to_string();
        assert!(req.validate().is_err());
        req.question = "  ".to_string();
        // validator sees length 2, so it passes length check, but handler's manual trim check would reject.
        // We test that the derived validator alone would pass whitespace-only, so handler must have extra check.
        // For this test we just ensure empty string is rejected by validator.
    }

    #[test]
    fn poll_fewer_than_two_options_rejected() {
        let mut req = valid_poll();
        req.options = vec!["only".to_string()];
        assert!(req.validate().is_err());
    }

    #[test]
    fn poll_more_than_twenty_options_rejected() {
        let mut req = valid_poll();
        req.options = (0..21).map(|i| format!("opt{i}")).collect();
        assert!(req.validate().is_err());
    }

    #[test]
    fn poll_invalid_max_selections_rejected() {
        let mut req = valid_poll();
        req.max_selections = 2;
        assert!(req.validate().is_err());
        req.max_selections = 0;
        assert!(req.validate().is_err());
    }

    #[test]
    fn poll_exactly_two_and_twenty_are_allowed() {
        let mut req = valid_poll();
        req.options = vec!["a".to_string(), "b".to_string()];
        assert!(req.validate().is_ok());
        req.options = (0..20).map(|i| format!("opt{i}")).collect();
        assert!(req.validate().is_ok());
    }
    #[test]
    fn poll_question_whitespace_and_control_rejected_by_details() {
        let mut req = valid_poll();
        req.question = "  Best?".to_string();
        assert!(validate_poll_details(&req).is_err());
        req.question = "Best? ".to_string();
        assert!(validate_poll_details(&req).is_err());
        req.question = "Best\u{0000}?".to_string();
        assert!(validate_poll_details(&req).is_err());
        req.question = "   ".to_string();
        assert!(validate_poll_details(&req).is_err());
    }

    #[test]
    fn poll_option_empty_and_whitespace_rejected() {
        let mut req = valid_poll();
        req.options = vec!["A".to_string(), "".to_string()];
        assert!(validate_poll_details(&req).is_err());
        req.options = vec!["A".to_string(), "  ".to_string()];
        assert!(validate_poll_details(&req).is_err());
        req.options = vec![" A".to_string(), "B".to_string()];
        assert!(validate_poll_details(&req).is_err());
        req.options = vec!["A".to_string(), "B ".to_string()];
        assert!(validate_poll_details(&req).is_err());
    }

    #[test]
    fn poll_option_control_and_length_rejected() {
        let mut req = valid_poll();
        req.options = vec!["A\u{0007}".to_string(), "B".to_string()];
        assert!(validate_poll_details(&req).is_err());
        req.options = vec!["a".repeat(201), "B".to_string()];
        assert!(validate_poll_details(&req).is_err());
        req.options = vec!["a".repeat(200), "B".to_string()];
        assert!(validate_poll_details(&req).is_ok());
    }

    #[test]
    fn poll_details_accepts_valid() {
        assert!(validate_poll_details(&valid_poll()).is_ok());
    }

    #[test]
    fn poll_option_grapheme_limits_with_chinese_and_emoji() {
        // 200 Chinese graphemes should be accepted (600 bytes but 200 graphemes)
        let mut req = valid_poll();
        req.options = vec!["中".repeat(200), "B".to_string()];
        assert!(validate_poll_details(&req).is_ok());
        // 201 Chinese graphemes should be rejected
        req.options = vec!["中".repeat(201), "B".to_string()];
        assert!(validate_poll_details(&req).is_err());

        // Flag emoji: 200 flags = 200 graphemes
        req.options = vec!["🇩🇪".repeat(200), "B".to_string()];
        assert!(validate_poll_details(&req).is_ok());
        req.options = vec!["🇩🇪".repeat(201), "B".to_string()];
        assert!(validate_poll_details(&req).is_err());

        // ZWJ family: 200 families = 200 graphemes
        req.options = vec!["👩‍👩‍👧‍👦".repeat(200), "B".to_string()];
        assert!(validate_poll_details(&req).is_ok());
        req.options = vec!["👩‍👩‍👧‍👦".repeat(201), "B".to_string()];
        assert!(validate_poll_details(&req).is_err());

        // Combining sequence: e + combining acute = 1 grapheme
        req.options = vec!["e\u{301}".repeat(200), "B".to_string()];
        assert!(validate_poll_details(&req).is_ok());
        req.options = vec!["e\u{301}".repeat(201), "B".to_string()];
        assert!(validate_poll_details(&req).is_err());
    }

    #[test]
    fn poll_question_grapheme_boundaries() {
        let mut req = valid_poll();
        req.question = "a".repeat(499);
        assert!(req.validate().is_ok());
        req.question = "a".repeat(500);
        assert!(req.validate().is_ok());
        req.question = "a".repeat(501);
        assert!(req.validate().is_err());

        // Chinese 500
        req.question = "中".repeat(500);
        assert!(req.validate().is_ok());
        req.question = "中".repeat(501);
        assert!(req.validate().is_err());

        // Flag 500
        req.question = "🇩🇪".repeat(500);
        assert!(req.validate().is_ok());
        req.question = "🇩🇪".repeat(501);
        assert!(req.validate().is_err());

        // Combining
        req.question = "e\u{301}".repeat(500);
        assert!(req.validate().is_ok());
        req.question = "e\u{301}".repeat(501);
        assert!(req.validate().is_err());

        // ZWJ
        req.question = "👩‍👩‍👧‍👦".repeat(500);
        assert!(req.validate().is_ok());
        req.question = "👩‍👩‍👧‍👦".repeat(501);
        assert!(req.validate().is_err());
    }

    #[test]
    fn reaction_key_grapheme_boundaries() {
        let mut req = ReactRequest {
            key: "a".repeat(32),
            author_public_key: "pk".to_string(),
            author_signature: "sig".to_string(),
            challenge_response: "chal|nonce".to_string(),
        };
        assert!(req.validate().is_ok());
        req.key = "a".repeat(33);
        assert!(req.validate().is_err());

        // 32 flags = 32 graphemes but >32 bytes
        req.key = "🇩🇪".repeat(32);
        assert!(req.key.len() > 32);
        assert!(req.validate().is_ok());
        req.key = "🇩🇪".repeat(33);
        assert!(req.validate().is_err());

        // ZWJ 32
        req.key = "👩‍👩‍👧‍👦".repeat(32);
        assert!(req.validate().is_ok());
        req.key = "👩‍👩‍👧‍👦".repeat(33);
        assert!(req.validate().is_err());

        // Combining 32
        req.key = "e\u{301}".repeat(32);
        assert!(req.validate().is_ok());
        req.key = "e\u{301}".repeat(33);
        assert!(req.validate().is_err());
    }

    #[test]
    fn display_name_and_comment_grapheme_boundaries() {
        // Post comment display_name 50
        let mut post = PostCommentRequest {
            content: "hi".to_string(),
            media: None,
            display_name: "a".repeat(50),
            author_public_key: "pk".to_string(),
            author_signature: "sig".to_string(),
            reply_to: None,
            thread_root: None,
            challenge_response: "chal|nonce".to_string(),
        };
        assert!(post.validate().is_ok());
        post.display_name = "a".repeat(51);
        assert!(post.validate().is_err());
        post.display_name = "🇩🇪".repeat(50);
        assert!(post.validate().is_ok());
        post.display_name = "🇩🇪".repeat(51);
        assert!(post.validate().is_err());

        // Post content 5000 (allow empty when media present, but we test max)
        post.display_name = "Alice".to_string();
        post.content = "a".repeat(5000);
        assert!(post.validate().is_ok());
        post.content = "a".repeat(5001);
        assert!(post.validate().is_err());
        // Chinese 5000
        post.content = "中".repeat(5000);
        assert!(post.validate().is_ok());
        post.content = "中".repeat(5001);
        assert!(post.validate().is_err());
        // Flag 5000
        post.content = "🇩🇪".repeat(5000);
        assert!(post.validate().is_ok());
        post.content = "🇩🇪".repeat(5001);
        assert!(post.validate().is_err());

        // Update content 1-5000
        let mut upd = UpdateCommentRequest {
            content: "a".repeat(5000),
            author_public_key: "pk".to_string(),
            author_signature: "sig".to_string(),
            challenge_response: "chal|nonce".to_string(),
        };
        assert!(upd.validate().is_ok());
        upd.content = "a".repeat(5001);
        assert!(upd.validate().is_err());
        upd.content = "".to_string();
        assert!(upd.validate().is_err());
        upd.content = "🇩🇪".repeat(5000);
        assert!(upd.validate().is_ok());

        // Location description 0-255
        let mut loc = LocationRequest {
            geo_uri: "geo:30,120".to_string(),
            description: Some("a".repeat(255)),
            display_name: "Alice".to_string(),
            author_public_key: "pk".to_string(),
            author_signature: "sig".to_string(),
            reply_to: None,
            thread_root: None,
            challenge_response: "chal|nonce".to_string(),
        };
        assert!(loc.validate().is_ok());
        loc.description = Some("a".repeat(256));
        assert!(loc.validate().is_err());
        loc.description = Some("".to_string());
        assert!(loc.validate().is_ok()); // empty allowed
        loc.description = Some("中".repeat(255));
        assert!(loc.validate().is_ok());
        loc.description = Some("中".repeat(256));
        assert!(loc.validate().is_err());
        loc.description = Some("🇩🇪".repeat(255));
        assert!(loc.validate().is_ok());
        loc.description = Some("🇩🇪".repeat(256));
        assert!(loc.validate().is_err());
        loc.description = None;
        assert!(loc.validate().is_ok());
    }

    #[test]
    fn basic_unicode_grapheme_len() {
        use crate::validation::grapheme_len;
        assert_eq!(grapheme_len("a"), 1);
        assert_eq!(grapheme_len("é"), 1);
        assert_eq!(grapheme_len("e\u{301}"), 1);
        assert_eq!(grapheme_len("🇩🇪"), 1);
        assert_eq!(grapheme_len("👩‍👩‍👧‍👦"), 1);
        assert_eq!(grapheme_len("中"), 1);
        // Mixed
        assert_eq!(grapheme_len("aé中"), 3);
        assert_eq!(grapheme_len("a e\u{301} b"), 5); // a, space, e-acute, space, b? Actually spaces are graphemes
        // Verify that ascii and CJK each count as 1
        assert_eq!(grapheme_len(&"a".repeat(10)), 10);
        assert_eq!(grapheme_len(&"中".repeat(10)), 10);
    }
}

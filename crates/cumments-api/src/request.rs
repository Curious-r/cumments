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
    /// Optional thread filter: when present, only active messages whose
    /// `thread_root` equals this event ID are returned. The root itself
    /// (where `thread_root` is `NULL`) is naturally excluded and `total`
    /// counts active replies in the thread.
    #[validate(length(min = 1, max = 255))]
    pub thread_root: Option<String>,
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

/// Request DTO for submitting a desired vote on a poll.
///
/// `option_ids` is an unordered selection set: it is deduplicated and
/// byte-wise sorted during semantic normalization, so duplicate or reordered
/// ids denote the same vote. An empty array is an explicit unvote.
#[derive(Debug, Deserialize, Validate)]
pub struct VoteRequest {
    /// Desired selections; empty is an explicit unvote.
    #[serde(default)]
    pub option_ids: Vec<String>,
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

/// One caller-authored answer in a Create Poll request.
///
/// `id` is an opaque, case-sensitive token validated by the frozen answer-id
/// syntax at the semantic layer; `text` is its display label.
#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct PollAnswerRequest {
    #[validate(length(min = 1, max = 64))]
    pub id: String,
    #[validate(custom(function = "crate::validation::validate_poll_answer_text"))]
    pub text: String,
}

/// Request DTO for creating a Poll (`POST .../polls`).
///
/// The HTTP body is transport only: the signed semantic operation is built
/// from these fields and never from the raw JSON. `kind` is the frozen
/// semantic value (`"disclosed"` / `"undisclosed"`); `max_selections` must be
/// between 1 and the number of answers.
#[derive(Debug, Deserialize, Validate)]
pub struct CreatePollRequest {
    #[validate(custom(function = "crate::validation::validate_poll_question"))]
    pub question: String,
    #[validate(length(min = 2, max = 20))]
    pub answers: Vec<PollAnswerRequest>,
    pub kind: cumments_core::poll::PollSemanticKind,
    #[validate(range(min = 1, max = 20))]
    pub max_selections: u64,
    /// Display name written to the virtual user's Matrix profile. Presentation
    /// data; deliberately not covered by the signature.
    #[validate(custom(function = "crate::validation::validate_display_name"))]
    pub display_name: String,
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    /// Direct parent comment (`$event:hs`). Orthogonal to `thread_root`.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Thread root (`$event:hs`). Orthogonal to `reply_to`.
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

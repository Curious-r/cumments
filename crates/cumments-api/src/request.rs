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

/// Request DTO for posting a comment.
#[derive(Debug, Deserialize, Validate)]
pub struct PostCommentRequest {
    #[validate(length(max = 5000))]
    pub content: String,
    /// Optional media attachment; when present the signature covers
    /// `media.url` and `content` is only the fallback filename.
    #[serde(default)]
    pub media: Option<CommentMedia>,
    /// Display name to write to the virtual user's Matrix profile. It is
    /// presentation data and is deliberately not covered by the author
    /// signature; the signed payload covers only content and reply relation.
    #[validate(length(min = 1, max = 50))]
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

/// Request DTO for deleting a comment. The target `comment_id` travels as a
/// query parameter, so the body carries only the author proof.
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
    /// Matrix event ID of the comment to edit. Required when calling the
    /// collection endpoint; ignored/optional on the legacy path endpoint.
    #[serde(default)]
    pub comment_id: Option<String>,
    #[validate(length(min = 1, max = 5000))]
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
    #[validate(length(min = 1, max = 32))]
    pub key: String,
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

/// Request DTO for posting a location. Like `PostCommentRequest`, it may
/// carry `reply_to` / `thread_root` so locations can start or join threads.
#[derive(Debug, Deserialize, Validate)]
pub struct LocationRequest {
    #[validate(length(min = 4, max = 512))]
    pub geo_uri: String,
    #[validate(length(min = 0, max = 255))]
    #[serde(default)]
    pub description: Option<String>,
    /// Display name to write to the virtual user's Matrix profile. It is
    /// presentation data and is deliberately not covered by the author
    /// signature; the signed payload covers only the geo URI.
    #[validate(length(min = 1, max = 50))]
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

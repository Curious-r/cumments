//! Request and response DTOs for the Cumments API.

use cumments_core::models::{CommentMedia, Message};
use serde::{Deserialize, Serialize};
use validator::Validate;

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

#[derive(Debug, Serialize)]
pub struct PaginationMeta {
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
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
    #[validate(length(min = 1, max = 50))]
    pub display_name: String,
    /// Ed25519 public key of the author (base64url, 32 bytes raw).
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    /// Ed25519 signature over the canonical POST message.
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    pub reply_to: Option<String>,
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

/// Request DTO for posting a location.
#[derive(Debug, Deserialize, Validate)]
pub struct LocationRequest {
    #[validate(length(min = 4, max = 512))]
    pub geo_uri: String,
    #[validate(length(min = 0, max = 255))]
    #[serde(default)]
    pub description: Option<String>,
    #[validate(length(min = 1, max = 50))]
    pub display_name: String,
    #[validate(length(min = 1, max = 128))]
    pub author_public_key: String,
    #[validate(length(min = 1, max = 256))]
    pub author_signature: String,
    #[validate(length(min = 1, max = 1024))]
    pub challenge_response: String,
}

//! Idempotency metadata for visitor media uploads.
//!
//! Uploads are synchronous writes that return an `mxc://` URL, so they
//! cannot ride the submission queue. The idempotency record is stored
//! separately from `media_uploads`: the upload-provenance record must stay
//! stable once a comment or avatar references it, while the key may be reused
//! after 24 hours.

use chrono::{DateTime, Utc};

/// How long an upload idempotency key stays valid, aligned with comment
/// write idempotency retention.
pub const MEDIA_UPLOAD_IDEMPOTENCY_RETENTION: chrono::Duration = chrono::Duration::hours(24);

/// Builds the canonical signature envelope signed for a site-scoped avatar
/// upload.
///
/// Format: `["UPLOAD", site_id, mime, filename, body_hash, challenge]`. Unlike
/// the page-scoped comment-media upload envelope, the avatar upload is
/// site-scoped and deliberately carries no page slug.
pub fn avatar_upload_signature_message(
    site_id: &str,
    mime: &str,
    filename: &str,
    body_hash: &str,
    challenge: &str,
) -> String {
    crate::identity::signature_message(&[
        Some("UPLOAD"),
        Some(site_id),
        Some(mime),
        Some(filename),
        Some(body_hash),
        Some(challenge),
    ])
}

/// Client-supplied idempotency identity for one upload request.
#[derive(Debug, Clone)]
pub struct MediaUploadIdempotencyInput {
    pub key: String,
    pub request_fingerprint: String,
}

/// A live (unexpired) media upload idempotency record.
#[derive(Debug, Clone)]
pub struct MediaUploadIdempotency {
    pub request_fingerprint: String,
    pub mxc_url: String,
    pub created_at: DateTime<Utc>,
}

/// Result of atomically recording an upload and its idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaUploadIdempotencyOutcome {
    /// This request's upload was recorded.
    Created { mxc_url: String },
    /// Another request with the same key and fingerprint already won; return
    /// its URL and discard the upload this request just made.
    Replayed { mxc_url: String },
    /// The key is bound to a different request fingerprint.
    Reused,
}

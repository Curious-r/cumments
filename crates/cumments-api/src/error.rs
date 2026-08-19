//! API error types.
//!
//! Error responses follow RFC 9457 (Problem Details for HTTP APIs). Every
//! problem type has a stable `type` URI under
//! `https://cumments.curious.host/problems/{slug}`; the `code`
//! member is the short machine-readable slug of that URI, and `title` is the
//! stable human-readable name of the problem type.

use axum::{
    Json,
    http::{
        HeaderValue, StatusCode,
        header::{CONTENT_TYPE, RETRY_AFTER},
    },
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Base URI of the documented problem types (GitHub Pages docs site with custom domain).
pub const PROBLEM_TYPE_BASE: &str = "https://cumments.curious.host/problems";

/// Stable machine-readable identifiers for every problem type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemType {
    InvalidPoW,
    InvalidSignature,
    Validation,
    NotFound,
    Unauthorized,
    NotManageable,
    MethodNotAllowed,
    BadRequest,
    Conflict,
    RateLimited,
    IdempotencyKeyRequired,
    InvalidIdempotencyKey,
    IdempotencyReused,
    SiteVerificationRequired,
    SiteNotRegistered,
    SiteRetired,
    SiteOriginDenied,
    SiteSignatureInvalid,
    Internal,
}

impl ProblemType {
    pub const ALL: [Self; 19] = [
        Self::InvalidPoW,
        Self::InvalidSignature,
        Self::Validation,
        Self::NotFound,
        Self::Unauthorized,
        Self::NotManageable,
        Self::MethodNotAllowed,
        Self::BadRequest,
        Self::Conflict,
        Self::RateLimited,
        Self::IdempotencyKeyRequired,
        Self::InvalidIdempotencyKey,
        Self::IdempotencyReused,
        Self::SiteVerificationRequired,
        Self::SiteNotRegistered,
        Self::SiteRetired,
        Self::SiteOriginDenied,
        Self::SiteSignatureInvalid,
        Self::Internal,
    ];

    /// Short identifier shared by the `type` URI and the `code` member.
    pub fn slug(self) -> &'static str {
        match self {
            Self::InvalidPoW => "invalid-pow",
            Self::InvalidSignature => "invalid-signature",
            Self::Validation => "validation-error",
            Self::NotFound => "not-found",
            Self::Unauthorized => "unauthorized",
            Self::NotManageable => "not-manageable",
            Self::MethodNotAllowed => "method-not-allowed",
            Self::BadRequest => "bad-request",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate-limited",
            Self::IdempotencyKeyRequired => "idempotency-key-required",
            Self::InvalidIdempotencyKey => "invalid-idempotency-key",
            Self::IdempotencyReused => "idempotency-key-reused",
            Self::SiteVerificationRequired => "site-verification-required",
            Self::SiteNotRegistered => "site-not-registered",
            Self::SiteRetired => "site-retired",
            Self::SiteOriginDenied => "site-origin-denied",
            Self::SiteSignatureInvalid => "site-signature-invalid",
            Self::Internal => "internal-error",
        }
    }

    /// Canonical `type` URI for this problem type.
    pub fn type_uri(self) -> String {
        format!("{PROBLEM_TYPE_BASE}/#{}", self.slug())
    }

    /// Stable, per-type title (does not change between occurrences).
    pub fn title(self) -> &'static str {
        match self {
            Self::InvalidPoW => "Invalid Proof-of-Work response",
            Self::InvalidSignature => "Invalid author signature",
            Self::Validation => "Input validation failed",
            Self::NotFound => "Resource not found",
            Self::Unauthorized => "Unauthorized",
            Self::NotManageable => "Comment not manageable",
            Self::MethodNotAllowed => "Method not allowed",
            Self::BadRequest => "Bad request",
            Self::Conflict => "Conflict",
            Self::RateLimited => "Rate limit exceeded",
            Self::IdempotencyKeyRequired => "Idempotency-Key required",
            Self::InvalidIdempotencyKey => "Invalid Idempotency-Key",
            Self::IdempotencyReused => "Idempotency-Key reused",
            Self::SiteVerificationRequired => "Site verification required",
            Self::SiteNotRegistered => "Site not registered",
            Self::SiteRetired => "Site retired",
            Self::SiteOriginDenied => "Site origin denied",
            Self::SiteSignatureInvalid => "Site signature invalid",
            Self::Internal => "Internal server error",
        }
    }
}

/// RFC 9457 problem details body.
#[derive(Serialize)]
pub struct ErrorResponse {
    #[serde(rename = "type")]
    pub type_: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

// --- Error Handling ---

pub enum AppError {
    InvalidPoW,
    InvalidSignature,
    Validation(validator::ValidationErrors),
    NotFound(String),
    MethodNotAllowed,
    Unauthorized(String),
    NotManageable(String),
    BadRequest(String),
    Conflict(String),
    TooManyRequests {
        detail: String,
        /// Conservative `Retry-After` value in seconds. This is the
        /// endpoint's constant rate-limit window, not the exact remaining
        /// time for this client key.
        retry_after_seconds: u64,
    },
    IdempotencyKeyRequired(String),
    InvalidIdempotencyKey(String),
    IdempotencyReused,
    SiteVerificationRequired(String),
    SiteNotRegistered(String),
    SiteRetired(String),
    SiteOriginDenied(String),
    SiteSignatureInvalid(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let retry_after_seconds = match &self {
            AppError::TooManyRequests {
                retry_after_seconds,
                ..
            } => Some(*retry_after_seconds),
            _ => None,
        };
        let (status, detail, code, details) = match self {
            AppError::InvalidPoW => (
                StatusCode::FORBIDDEN,
                "Invalid Proof-of-Work response.".to_string(),
                ProblemType::InvalidPoW,
                None,
            ),
            AppError::InvalidSignature => (
                StatusCode::FORBIDDEN,
                "Invalid author signature.".to_string(),
                ProblemType::InvalidSignature,
                None,
            ),
            AppError::Validation(errs) => (
                StatusCode::BAD_REQUEST,
                "Input validation failed.".to_string(),
                ProblemType::Validation,
                serde_json::to_value(errs).ok(),
            ),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, ProblemType::NotFound, None),
            AppError::Unauthorized(msg) => {
                (StatusCode::FORBIDDEN, msg, ProblemType::Unauthorized, None)
            }
            AppError::NotManageable(msg) => {
                (StatusCode::FORBIDDEN, msg, ProblemType::NotManageable, None)
            }
            AppError::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                "Method not allowed. Use QUERY for queries, POST for submissions.".to_string(),
                ProblemType::MethodNotAllowed,
                None,
            ),
            AppError::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, msg, ProblemType::BadRequest, None)
            }
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg, ProblemType::Conflict, None),
            AppError::TooManyRequests { detail, .. } => (
                StatusCode::TOO_MANY_REQUESTS,
                detail,
                ProblemType::RateLimited,
                None,
            ),
            AppError::IdempotencyKeyRequired(msg) => (
                StatusCode::BAD_REQUEST,
                msg,
                ProblemType::IdempotencyKeyRequired,
                None,
            ),
            AppError::InvalidIdempotencyKey(msg) => (
                StatusCode::BAD_REQUEST,
                msg,
                ProblemType::InvalidIdempotencyKey,
                None,
            ),
            AppError::IdempotencyReused => (
                StatusCode::CONFLICT,
                "This Idempotency-Key was already used with a different request.".to_string(),
                ProblemType::IdempotencyReused,
                None,
            ),
            AppError::SiteVerificationRequired(msg) => (
                StatusCode::FORBIDDEN,
                msg,
                ProblemType::SiteVerificationRequired,
                None,
            ),
            AppError::SiteNotRegistered(msg) => (
                StatusCode::NOT_FOUND,
                msg,
                ProblemType::SiteNotRegistered,
                None,
            ),
            AppError::SiteRetired(msg) => (StatusCode::GONE, msg, ProblemType::SiteRetired, None),
            AppError::SiteOriginDenied(msg) => (
                StatusCode::FORBIDDEN,
                msg,
                ProblemType::SiteOriginDenied,
                None,
            ),
            AppError::SiteSignatureInvalid(msg) => (
                StatusCode::FORBIDDEN,
                msg,
                ProblemType::SiteSignatureInvalid,
                None,
            ),
            AppError::Internal(msg) => {
                // Log the detail server-side; never echo it to clients.
                tracing::error!("Internal error: {msg}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error.".to_string(),
                    ProblemType::Internal,
                    None,
                )
            }
        };

        let body = ErrorResponse {
            type_: code.type_uri(),
            title: code.title().to_string(),
            status: status.as_u16(),
            detail,
            code: code.slug().to_string(),
            details,
        };

        let mut response = (status, Json(body)).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let Some(seconds) = retry_after_seconds {
            response.headers_mut().insert(
                RETRY_AFTER,
                HeaderValue::from_str(&seconds.to_string())
                    .expect("numeric Retry-After is a valid header value"),
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[tokio::test]
    async fn error_response_is_rfc9457_problem_details() {
        let response = AppError::IdempotencyReused.into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );

        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(body["type"], ProblemType::IdempotencyReused.type_uri());
        assert_eq!(body["title"], "Idempotency-Key reused");
        assert_eq!(body["status"], 409);
        assert_eq!(
            body["detail"],
            "This Idempotency-Key was already used with a different request."
        );
        assert_eq!(body["code"], "idempotency-key-reused");
    }

    #[test]
    fn rate_limited_response_carries_retry_after() {
        let response = AppError::TooManyRequests {
            detail: "slow down".to_string(),
            retry_after_seconds: 3600,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers().get(RETRY_AFTER).unwrap(),
            "3600",
            "429 must advertise the endpoint's fixed retry window"
        );
    }

    #[test]
    fn every_code_has_a_documented_anchor() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/problems/index.md");
        let docs = std::fs::read_to_string(path).expect("problems doc must exist");
        for code in ProblemType::ALL {
            assert!(
                docs.contains(&format!("{{#{}}}", code.slug())),
                "docs/problems/index.md is missing anchor for `{}`",
                code.slug()
            );
            assert!(
                docs.contains(&format!("{}/#{}", PROBLEM_TYPE_BASE, code.slug())),
                "docs/problems/index.md is missing the canonical type URI for `{}`",
                code.slug()
            );
        }
    }

    #[test]
    fn problem_type_base_matches_mkdocs_site_url() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/mkdocs.yml");
        let mkdocs = std::fs::read_to_string(path).expect("mkdocs.yml must exist");
        let site_root = PROBLEM_TYPE_BASE
            .strip_suffix("/problems")
            .expect("PROBLEM_TYPE_BASE must end with /problems");
        assert!(
            mkdocs.contains(&format!("site_url: {site_root}/")),
            "mkdocs.yml site_url must match PROBLEM_TYPE_BASE"
        );
    }

    #[test]
    fn code_slugs_are_unique() {
        let slugs = ProblemType::ALL
            .iter()
            .map(|c| c.slug())
            .collect::<HashSet<_>>();
        assert_eq!(slugs.len(), ProblemType::ALL.len());
    }
}

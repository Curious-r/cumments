//! API error types.
//!
//! Error responses follow RFC 9457 (Problem Details for HTTP APIs). Every
//! problem type has a stable `type` URI under
//! `https://curious-r.github.io/cumments/problems/{slug}`; the `error_code`
//! member is the short machine-readable slug of that URI, and `title` is the
//! stable human-readable name of the problem type.

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Base URI of the documented problem types (GitHub Pages docs site).
pub const PROBLEM_TYPE_BASE: &str = "https://curious-r.github.io/cumments/problems";

/// Stable machine-readable identifiers for every problem type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidPow,
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
    SiteOriginDenied,
    SiteSignatureInvalid,
    Internal,
}

impl ErrorCode {
    pub const ALL: [Self; 17] = [
        Self::InvalidPow,
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
        Self::SiteOriginDenied,
        Self::SiteSignatureInvalid,
        Self::Internal,
    ];

    /// Short identifier shared by the `type` URI and the `error_code` member.
    pub fn slug(self) -> &'static str {
        match self {
            Self::InvalidPow => "invalid-pow",
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
            Self::InvalidPow => "Invalid Proof-of-Work response",
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
    #[serde(rename = "error_code")]
    pub error_code: String,
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
    TooManyRequests(String),
    IdempotencyKeyRequired(String),
    InvalidIdempotencyKey(String),
    IdempotencyReused,
    SiteVerificationRequired(String),
    SiteOriginDenied(String),
    SiteSignatureInvalid(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, detail, error_code, details) = match self {
            AppError::InvalidPoW => (
                StatusCode::FORBIDDEN,
                "Invalid Proof-of-Work response.".to_string(),
                ErrorCode::InvalidPow,
                None,
            ),
            AppError::InvalidSignature => (
                StatusCode::FORBIDDEN,
                "Invalid author signature.".to_string(),
                ErrorCode::InvalidSignature,
                None,
            ),
            AppError::Validation(errs) => (
                StatusCode::BAD_REQUEST,
                "Input validation failed.".to_string(),
                ErrorCode::Validation,
                serde_json::to_value(errs).ok(),
            ),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, ErrorCode::NotFound, None),
            AppError::Unauthorized(msg) => {
                (StatusCode::FORBIDDEN, msg, ErrorCode::Unauthorized, None)
            }
            AppError::NotManageable(msg) => {
                (StatusCode::FORBIDDEN, msg, ErrorCode::NotManageable, None)
            }
            AppError::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                "Method not allowed. Use QUERY for queries, POST for submissions.".to_string(),
                ErrorCode::MethodNotAllowed,
                None,
            ),
            AppError::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, msg, ErrorCode::BadRequest, None)
            }
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg, ErrorCode::Conflict, None),
            AppError::TooManyRequests(msg) => (
                StatusCode::TOO_MANY_REQUESTS,
                msg,
                ErrorCode::RateLimited,
                None,
            ),
            AppError::IdempotencyKeyRequired(msg) => (
                StatusCode::BAD_REQUEST,
                msg,
                ErrorCode::IdempotencyKeyRequired,
                None,
            ),
            AppError::InvalidIdempotencyKey(msg) => (
                StatusCode::BAD_REQUEST,
                msg,
                ErrorCode::InvalidIdempotencyKey,
                None,
            ),
            AppError::IdempotencyReused => (
                StatusCode::CONFLICT,
                "This Idempotency-Key was already used with a different request.".to_string(),
                ErrorCode::IdempotencyReused,
                None,
            ),
            AppError::SiteVerificationRequired(msg) => (
                StatusCode::FORBIDDEN,
                msg,
                ErrorCode::SiteVerificationRequired,
                None,
            ),
            AppError::SiteOriginDenied(msg) => (
                StatusCode::FORBIDDEN,
                msg,
                ErrorCode::SiteOriginDenied,
                None,
            ),
            AppError::SiteSignatureInvalid(msg) => (
                StatusCode::FORBIDDEN,
                msg,
                ErrorCode::SiteSignatureInvalid,
                None,
            ),
            AppError::Internal(msg) => {
                // Log the detail server-side; never echo it to clients.
                tracing::error!("Internal error: {msg}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error.".to_string(),
                    ErrorCode::Internal,
                    None,
                )
            }
        };

        let body = ErrorResponse {
            type_: error_code.type_uri(),
            title: error_code.title().to_string(),
            status: status.as_u16(),
            detail,
            error_code: error_code.slug().to_string(),
            details,
        };

        let mut response = (status, Json(body)).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
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
        assert_eq!(
            body["type"],
            "https://curious-r.github.io/cumments/problems/#idempotency-key-reused"
        );
        assert_eq!(body["title"], "Idempotency-Key reused");
        assert_eq!(body["status"], 409);
        assert_eq!(
            body["detail"],
            "This Idempotency-Key was already used with a different request."
        );
        assert_eq!(body["error_code"], "idempotency-key-reused");
    }

    #[test]
    fn every_error_code_has_a_documented_anchor() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/problems/index.md");
        let docs = std::fs::read_to_string(path).expect("problems doc must exist");
        for code in ErrorCode::ALL {
            assert!(
                docs.contains(&format!("{{#{}}}", code.slug())),
                "docs/problems/index.md is missing anchor for `{}`",
                code.slug()
            );
        }
    }

    #[test]
    fn error_code_slugs_are_unique() {
        let slugs = ErrorCode::ALL
            .iter()
            .map(|c| c.slug())
            .collect::<HashSet<_>>();
        assert_eq!(slugs.len(), ErrorCode::ALL.len());
    }
}

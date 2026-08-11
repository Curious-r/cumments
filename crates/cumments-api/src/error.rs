//! API error types.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

// --- Error Handling ---

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

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
    SiteVerificationRequired(String),
    SiteOriginDenied(String),
    SiteSignatureInvalid(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_msg, code, details) = match self {
            AppError::InvalidPoW => (
                StatusCode::FORBIDDEN,
                "Invalid Proof-of-Work response.".to_string(),
                "INVALID_POW",
                None,
            ),
            AppError::InvalidSignature => (
                StatusCode::FORBIDDEN,
                "Invalid author signature.".to_string(),
                "INVALID_SIGNATURE",
                None,
            ),
            AppError::Validation(errs) => (
                StatusCode::BAD_REQUEST,
                "Input validation failed.".to_string(),
                "VALIDATION_ERROR",
                serde_json::to_value(errs).ok(),
            ),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, "NOT_FOUND", None),
            AppError::Unauthorized(msg) => (StatusCode::FORBIDDEN, msg, "UNAUTHORIZED", None),
            AppError::NotManageable(msg) => (StatusCode::FORBIDDEN, msg, "NOT_MANAGEABLE", None),
            AppError::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                "Method not allowed. Use QUERY for queries, POST for submissions.".to_string(),
                "METHOD_NOT_ALLOWED",
                None,
            ),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg, "BAD_REQUEST", None),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg, "CONFLICT", None),
            AppError::TooManyRequests(msg) => {
                (StatusCode::TOO_MANY_REQUESTS, msg, "RATE_LIMITED", None)
            }
            AppError::SiteVerificationRequired(msg) => (
                StatusCode::FORBIDDEN,
                msg,
                "SITE_VERIFICATION_REQUIRED",
                None,
            ),
            AppError::SiteOriginDenied(msg) => {
                (StatusCode::FORBIDDEN, msg, "SITE_ORIGIN_DENIED", None)
            }
            AppError::SiteSignatureInvalid(msg) => {
                (StatusCode::FORBIDDEN, msg, "SITE_SIGNATURE_INVALID", None)
            }
            AppError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                msg,
                "INTERNAL_ERROR",
                None,
            ),
        };

        let body = Json(ErrorResponse {
            error: error_msg,
            code: code.to_string(),
            details,
        });

        (status, body).into_response()
    }
}

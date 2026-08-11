//! Health and PoW challenge route handlers.

use crate::ApiState;
use crate::request::ChallengeResponse;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

pub(crate) async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

/// The handler for generating a new PoW challenge.
pub(crate) async fn get_challenge_handler(State(state): State<ApiState>) -> impl IntoResponse {
    let challenge = state.pow.generate_challenge();
    let response = ChallengeResponse {
        prefix: challenge.prefix,
        difficulty: challenge.difficulty,
    };
    (StatusCode::OK, Json(response))
}

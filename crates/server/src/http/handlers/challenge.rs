use crate::state::AppState;
use axum::{extract::State, Json};

pub async fn get_challenge(State(state): State<AppState>) -> Json<serde_json::Value> {
    let secret = state.pow.generate_challenge();
    Json(serde_json::json!({ "secret": secret, "difficulty": 4 }))
}

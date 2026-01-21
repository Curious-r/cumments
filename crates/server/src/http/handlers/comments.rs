use axum::{
    extract::{Path, State},
    Json,
};
use domain::{AppCommand, SiteId};
use matrix_sdk::ruma::EventId;
use serde::Deserialize;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateCommentRequest {
    pub post_slug: String,
    pub content: String,
    pub nickname: String,
    pub email: Option<String>,
    pub guest_token: String,

    pub challenge_response: String,
    pub reply_to: Option<String>,
}

pub async fn list_comments(
    State(state): State<AppState>,
    Path((site_id_str, slug)): Path<(String, String)>,
) -> Result<Json<Vec<domain::Comment>>, (axum::http::StatusCode, String)> {
    if SiteId::new(&site_id_str).is_err() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "Invalid Site ID format".to_string(),
        ));
    }

    let comments = state
        .db
        .list_comments(&site_id_str, &slug)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(comments))
}

pub async fn post_comment(
    State(state): State<AppState>,
    Path(site_id_str): Path<String>,
    Json(payload): Json<CreateCommentRequest>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    let site_id = SiteId::new(site_id_str).map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e))?;

    if let Some(ref reply_id) = payload.reply_to {
        if EventId::parse(reply_id).is_err() {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                format!("Invalid reply_to ID format: {}", reply_id),
            ));
        }
    }

    let parts: Vec<&str> = payload.challenge_response.split('|').collect();
    if parts.len() != 2 || !state.pow.verify(parts[0], parts[1]) {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            "Invalid PoW Challenge".to_string(),
        ));
    }

    let cmd = AppCommand::SendComment {
        site_id,
        post_slug: payload.post_slug,
        content: payload.content,
        nickname: payload.nickname,
        email: payload.email,
        guest_token: payload.guest_token,
        reply_to: payload.reply_to,
    };

    if state.sender.send(cmd).await.is_err() {
        return Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Worker closed".to_string(),
        ));
    }
    Ok(Json("Accepted"))
}

use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use domain::{AppCommand, SiteId};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct InitRoomRequest {
    pub site_id: String,
    pub slug: String,
}

pub async fn delete_comment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((site_id_str, slug, comment_id)): Path<(String, String, String)>,
) -> Result<Json<&'static str>, (StatusCode, String)> {
    let auth_header = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "Missing Authorization header".into(),
        ))?;
    let expected_token = format!("Bearer {}", state.admin_token);
    if auth_header != expected_token {
        return Err((StatusCode::FORBIDDEN, "Invalid Admin Token".into()));
    }

    let site_id = SiteId::new(site_id_str).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let cmd = AppCommand::RedactComment {
        site_id,
        post_slug: slug,
        comment_id,
        reason: Some("Admin deleted via API".into()),
    };

    state
        .matrix
        .send(cmd)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json("Deleted"))
}

pub async fn init_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<InitRoomRequest>,
) -> Result<Json<&'static str>, (StatusCode, String)> {
    let auth_header = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "Missing Token".into()))?;
    if auth_header != format!("Bearer {}", state.admin_token) {
        return Err((StatusCode::FORBIDDEN, "Invalid Token".into()));
    }

    let site_id = SiteId::new(payload.site_id).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let cmd = AppCommand::EnsureRoom {
        site_id,
        post_slug: payload.slug,
    };

    state
        .matrix
        .send(cmd)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json("Room Created/Ensured"))
}


use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use crate::state::AppState;
use adapter::CommandEnvelope;
use domain::{AppCommand, SiteId};
use tokio::sync::oneshot;

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

    // 等待反馈
    let (tx, rx) = oneshot::channel();
    let envelope = CommandEnvelope { cmd, resp: tx };

    state.sender.send(envelope).await.map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "Worker closed".to_string())
    })?;

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(_))) => Ok(Json("Deleted")),
        Ok(Ok(Err(e))) => Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Matrix Error: {}", e))),
        _ => Err((StatusCode::GATEWAY_TIMEOUT, "Timeout".into())),
    }
}

use crate::state::AppState;
use adapter::CommandEnvelope;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use domain::{AppCommand, SiteId};
use matrix_sdk::ruma::EventId;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

#[derive(Deserialize)]
pub struct CreateCommentRequest {
    pub post_slug: String,
    pub content: String,
    pub nickname: String,
    pub email: Option<String>,
    pub guest_token: String,
    pub challenge_response: String,
    pub reply_to: Option<String>,
    pub txn_id: Option<String>,
}

#[derive(Deserialize)]
pub struct PaginationQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Serialize)]
pub struct PaginatedResponse {
    pub data: Vec<domain::Comment>,
    pub meta: PaginationMeta,
}

#[derive(Serialize)]
pub struct PaginationMeta {
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
    pub room_alias: String,
    pub matrix_to_link: String,
}

async fn send_cmd_and_wait(
    sender: &tokio::sync::mpsc::Sender<CommandEnvelope>,
    cmd: AppCommand,
) -> Result<(), (axum::http::StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let envelope = CommandEnvelope { cmd, resp: tx };

    sender.send(envelope).await.map_err(|_| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Service Unavailable (Worker Channel Closed)".to_string(),
        )
    })?;

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(_))) => Ok(()),
        Ok(Ok(Err(e))) => Err((
            axum::http::StatusCode::BAD_REQUEST,
            format!("Operation failed: {}", e),
        )),
        Ok(Err(_)) => Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Worker dropped the response channel".to_string(),
        )),
        Err(_) => Err((
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            "Operation timed out".to_string(),
        )),
    }
}

pub async fn list_comments(
    State(state): State<AppState>,
    Path((site_id_str, slug)): Path<(String, String)>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<Json<PaginatedResponse>, (axum::http::StatusCode, String)> {
    if SiteId::new(&site_id_str).is_err() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "Invalid Site ID format".to_string(),
        ));
    }

    let page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);
    let limit = per_page;
    let offset = (page - 1) * per_page;

    let (comments, total) = state
        .db
        .list_comments(&site_id_str, &slug, limit, offset)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let total_pages = if total > 0 {
        (total + per_page - 1) / per_page
    } else {
        0
    };

    let room_alias = format!("#{}_{}:{}", site_id_str, slug, state.server_name);
    let matrix_to_link = format!("https://matrix.to/#/{}", room_alias);

    Ok(Json(PaginatedResponse {
        data: comments,
        meta: PaginationMeta {
            total,
            page,
            per_page,
            total_pages,
            room_alias,
            matrix_to_link,
        },
    }))
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
        txn_id: payload.txn_id,
    };

    send_cmd_and_wait(&state.sender, cmd).await?;

    Ok(Json("Accepted"))
}

#[derive(Deserialize)]
pub struct DeleteCommentRequest {
    pub user_fingerprint: String,
}

pub async fn delete_comment(
    State(state): State<AppState>,
    Path((site_id_str, slug, comment_id)): Path<(String, String, String)>,
    Json(payload): Json<DeleteCommentRequest>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    let site_id = SiteId::new(site_id_str).map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e))?;

    let comment_opt = state
        .db
        .get_comment(&comment_id)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(c) = comment_opt {
        if c.author_fingerprint.as_ref() != Some(&payload.user_fingerprint) {
            return Err((
                axum::http::StatusCode::FORBIDDEN,
                "Permission Denied: Fingerprint mismatch".to_string(),
            ));
        }
        if c.is_redacted {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                "Already deleted".to_string(),
            ));
        }
    } else {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            "Comment not found".to_string(),
        ));
    }

    let cmd = AppCommand::UserDeleteComment {
        site_id,
        post_slug: slug,
        comment_id,
        user_fingerprint: payload.user_fingerprint,
    };

    send_cmd_and_wait(&state.sender, cmd).await?;

    Ok(Json("Deleted"))
}

#[derive(Deserialize)]
pub struct EditCommentRequest {
    pub content: String,
    pub user_fingerprint: String,
}

pub async fn edit_comment(
    State(state): State<AppState>,
    Path((site_id_str, slug, comment_id)): Path<(String, String, String)>,
    Json(payload): Json<EditCommentRequest>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    let site_id = SiteId::new(site_id_str).map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e))?;

    let cmd = AppCommand::UserEditComment {
        site_id,
        post_slug: slug,
        comment_id,
        content: payload.content,
        user_fingerprint: payload.user_fingerprint,
    };

    send_cmd_and_wait(&state.sender, cmd).await?;

    Ok(Json("Edited"))
}

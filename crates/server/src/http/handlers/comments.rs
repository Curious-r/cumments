use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use domain::{AppCommand, SiteId};
use serde::{Deserialize, Serialize};

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

    let matrix = state.matrix.clone();
    let site_id_cmd = SiteId::new_unchecked(site_id_str.clone());
    let slug_cmd = slug.clone();

    tokio::spawn(async move {
        let cmd = AppCommand::Backfill {
            site_id: site_id_cmd,
            post_slug: slug_cmd,
        };
        if let Err(e) = matrix.send(cmd).await {
            tracing::warn!("Failed to trigger backfill: {:?}", e);
        }
    });

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

    state
        .matrix
        .send(cmd)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json("Accepted"))
}

#[derive(Deserialize)]
pub struct GuestActionRequest {
    pub guest_token: String,
    pub email: Option<String>,
    pub content: Option<String>,
}

pub async fn delete_comment(
    State(state): State<AppState>,
    Path((site_id_str, slug, comment_id)): Path<(String, String, String)>,
    Json(payload): Json<GuestActionRequest>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    let site_id = SiteId::new(site_id_str).map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e))?;

    let calculated_fingerprint = domain::identity::compute_fingerprint(
        payload.email.as_deref(),
        &payload.guest_token,
        &state.identity_salt,
    );

    let comment_opt = state
        .db
        .get_comment(&comment_id)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(c) = comment_opt {
        if c.author_fingerprint.as_ref() != Some(&calculated_fingerprint) {
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
        user_fingerprint: calculated_fingerprint,
    };

    state
        .matrix
        .send(cmd)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json("Deleted"))
}

pub async fn edit_comment(
    State(state): State<AppState>,
    Path((site_id_str, slug, comment_id)): Path<(String, String, String)>,
    Json(payload): Json<GuestActionRequest>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    let site_id = SiteId::new(site_id_str).map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e))?;

    let new_content = payload.content.ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        "Missing content".to_string(),
    ))?;

    let calculated_fingerprint = domain::identity::compute_fingerprint(
        payload.email.as_deref(),
        &payload.guest_token,
        &state.identity_salt,
    );

    let comment_opt = state
        .db
        .get_comment(&comment_id)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(c) = comment_opt {
        if c.author_fingerprint.as_ref() != Some(&calculated_fingerprint) {
            return Err((
                axum::http::StatusCode::FORBIDDEN,
                "Permission Denied".to_string(),
            ));
        }
    } else {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            "Comment not found".to_string(),
        ));
    }

    let cmd = AppCommand::UserEditComment {
        site_id,
        post_slug: slug,
        comment_id,
        content: new_content,
        user_fingerprint: calculated_fingerprint,
    };

    state
        .matrix
        .send(cmd)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json("Edited"))
}

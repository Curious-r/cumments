use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::stream::Stream;
use matrix_sdk::ruma::EventId;
use serde::Deserialize;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

use crate::app_state::AppState;
use domain::{AppCommand, IngestEvent, SiteId};

// --- DTOs ---

#[derive(Deserialize)]
pub struct CreateCommentRequest {
    pub post_slug: String,
    pub content: String,
    pub nickname: String,
    pub challenge_response: String,
    pub reply_to: Option<String>,
}

// --- Handlers ---

pub async fn sse_handler(
    State(state): State<AppState>,
    Path((site_id_str, slug)): Path<(String, String)>,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    let rx = state.tx_ingest.subscribe();

    tracing::info!("SSE Connected: site={} slug={}", site_id_str, slug);

    let stream = BroadcastStream::new(rx).filter_map(move |result| match result {
        Ok(event) => match event {
            IngestEvent::CommentSaved {
                site_id: event_site_id,
                post_slug: event_slug,
                comment,
            } => {
                if event_site_id.as_str() == site_id_str && event_slug == slug {
                    let event_type = if comment.updated_at.is_some() {
                        "update_comment"
                    } else {
                        "new_comment"
                    };

                    Some(
                        Event::default()
                            .event(event_type)
                            .json_data(comment)
                            .map_err(|e| {
                                tracing::error!("SSE serialization error: {}", e);
                                axum::Error::new(e)
                            }),
                    )
                } else {
                    None
                }
            }
            IngestEvent::CommentDeleted {
                site_id: event_site_id,
                post_slug: event_slug,
                comment_id,
            } => {
                if event_site_id.as_str() == site_id_str && event_slug == slug {
                    Some(
                        Event::default()
                            .event("delete_comment")
                            .json_data(serde_json::json!({ "id": comment_id }))
                            .map_err(|e| {
                                tracing::error!("SSE serialization error: {}", e);
                                axum::Error::new(e)
                            }),
                    )
                } else {
                    None
                }
            }
        },
        Err(_lagged) => {
            tracing::warn!("SSE Client lagged for {}/{}", site_id_str, slug);
            None
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}

pub async fn get_challenge(State(state): State<AppState>) -> Json<serde_json::Value> {
    let secret = state.pow.generate_challenge();
    Json(serde_json::json!({ "secret": secret, "difficulty": 4 }))
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

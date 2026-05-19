use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, post},
};
use cumments_core::{
    events::ProjectorEvent,
    intents::{DeleteCommentIntent, PostCommentIntent},
    models::{Comment, PostSlug, SiteId},
    ports::{CommentStore, IntentStore, SiteStore},
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Arc};
use tokio::sync::broadcast;

pub mod pow;

// Define a new trait that combines the store traits for API use.
pub trait ApiStore: CommentStore + IntentStore + SiteStore + Send + Sync {}
impl<T: CommentStore + IntentStore + SiteStore + Send + Sync> ApiStore for T {}

// The shared state for our API.
#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<dyn ApiStore>,
    pub pow: Arc<pow::Pow>,
    pub event_bus: broadcast::Sender<ProjectorEvent>,
}

/// The query parameters for the `GET /api/comments` endpoint.
#[derive(Debug, Deserialize)]
pub struct GetCommentsQuery {
    pub site_id: String,
    pub post_slug: String,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Serialize)]
pub struct PaginatedResponse {
    pub data: Vec<Comment>,
    pub meta: PaginationMeta,
}

#[derive(Serialize)]
pub struct PaginationMeta {
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}

/// The response for the `GET /api/challenge` endpoint.
#[derive(Serialize)]
pub struct ChallengeResponse {
    pub prefix: String,
    pub difficulty: u32,
}

/// Request DTO for posting a comment.
#[derive(Debug, Deserialize)]
pub struct PostCommentRequest {
    pub site_id: SiteId,
    pub post_slug: PostSlug,
    pub content: String,
    pub nickname: String,
    pub email: Option<String>,
    pub author_fingerprint: String,
    pub reply_to: Option<String>,
    pub challenge_response: String,
}

/// Request DTO for deleting a comment.
#[derive(Debug, Deserialize)]
pub struct DeleteCommentRequest {
    pub site_id: SiteId,
    pub post_slug: PostSlug,
    pub event_id: String,
    pub author_fingerprint: String,
    pub challenge_response: String,
}

/// Builds the Axum router for the API.
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route(
            "/api/comments",
            post(post_comment_handler).get(get_comments_handler),
        )
        .route("/api/comments/:comment_id", delete(delete_comment_handler))
        .route("/api/:site_id/comments/:post_slug/sse", get(sse_handler))
        .route("/api/challenge", get(get_challenge_handler))
        .with_state(state)
}

/// The handler for generating a new PoW challenge.
async fn get_challenge_handler(State(state): State<ApiState>) -> impl IntoResponse {
    let challenge = state.pow.generate_challenge();
    let response = ChallengeResponse {
        prefix: challenge.prefix,
        difficulty: challenge.difficulty,
    };
    (StatusCode::OK, Json(response))
}

/// The handler for fetching all comments for a given page.
async fn get_comments_handler(
    State(state): State<ApiState>,
    Query(query): Query<GetCommentsQuery>,
) -> impl IntoResponse {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = per_page;
    let offset = (page - 1) * per_page;

    let site_id: SiteId = query.site_id.into();
    let post_slug: PostSlug = query.post_slug.into();

    match state
        .store
        .get_comments(&site_id, &post_slug, limit, offset)
        .await
    {
        Ok((comments, total)) => {
            let total_pages = if total > 0 {
                (total + per_page - 1) / per_page
            } else {
                0
            };

            let response = PaginatedResponse {
                data: comments,
                meta: PaginationMeta {
                    total,
                    page,
                    per_page,
                    total_pages,
                },
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to get comments: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to retrieve comments.",
            )
                .into_response()
        }
    }
}

/// The handler for receiving a new comment post.
async fn post_comment_handler(
    State(state): State<ApiState>,
    Json(req): Json<PostCommentRequest>,
) -> impl IntoResponse {
    // 1. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return (StatusCode::FORBIDDEN, "Invalid Proof-of-Work response.").into_response();
    }

    // 2. Create the business intent
    let intent = PostCommentIntent {
        site_id: req.site_id,
        post_slug: req.post_slug,
        content: req.content,
        nickname: req.nickname,
        email: req.email,
        author_fingerprint: req.author_fingerprint,
        reply_to: req.reply_to,
    };

    // 3. Save the intent for the reconciler
    match state.store.save_post_intent(&intent).await {
        Ok(_) => {
            tracing::info!("Successfully saved a new comment intent.");
            (
                StatusCode::ACCEPTED,
                "Comment received and queued for processing.",
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Failed to save comment intent: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to queue comment.",
            )
                .into_response()
        }
    }
}

/// The handler for receiving a new delete comment request.
async fn delete_comment_handler(
    State(state): State<ApiState>,
    Path(comment_id): Path<String>,
    Json(req): Json<DeleteCommentRequest>,
) -> impl IntoResponse {
    // 1. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return (StatusCode::FORBIDDEN, "Invalid Proof-of-Work response.").into_response();
    }

    // 2. Ensure consistency between path and body
    if req.event_id != comment_id {
        return (
            StatusCode::BAD_REQUEST,
            "Comment ID in path does not match ID in body.",
        )
            .into_response();
    }

    // 3. Create the business intent
    let intent = DeleteCommentIntent {
        site_id: req.site_id,
        post_slug: req.post_slug,
        event_id: req.event_id,
        author_fingerprint: req.author_fingerprint,
    };

    // 4. Save the intent for the reconciler
    match state.store.save_delete_intent(&intent).await {
        Ok(_) => {
            tracing::info!("Successfully saved a delete comment intent.");
            (
                StatusCode::ACCEPTED,
                "Delete request received and queued for processing.",
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Failed to save delete comment intent: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to queue delete request.",
            )
                .into_response()
        }
    }
}

/// SSE handler that streams projector events for a specific post.
async fn sse_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.event_bus.subscribe();

    let stream = async_stream::stream! {
        while let Ok(event) = rx.recv().await {
            // Filter events by site_id and post_slug
            let matches = match &event {
                ProjectorEvent::NewComment { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
                ProjectorEvent::CommentUpdated { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
                ProjectorEvent::CommentDeleted { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
            };

            if matches {
                if let Ok(json) = serde_json::to_string(&event) {
                    yield Ok(Event::default().data(json));
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

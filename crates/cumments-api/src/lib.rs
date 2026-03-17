use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use cumments_core::{
    intents::{DeleteCommentIntent, PostCommentIntent},
    models::Comment,
    ports::{CommentRepository, IntentRepository},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod pow;

// Define a new trait that combines the repository traits for API use.
pub trait ApiRepository: CommentRepository + IntentRepository + Send + Sync {}
impl<T: CommentRepository + IntentRepository + Send + Sync> ApiRepository for T {}

// The shared state for our API.
#[derive(Clone)]
pub struct ApiState {
    pub storage: Arc<dyn ApiRepository>,
    pub pow: Arc<pow::Pow>,
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

/// Builds the Axum router for the API.
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route(
            "/api/comments",
            post(post_comment_handler).get(get_comments_handler),
        )
        .route("/api/comments/:comment_id", delete(delete_comment_handler))
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

    match state
        .storage
        .get_comments(&query.site_id, &query.post_slug, limit, offset)
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
/// It deserializes the user's request into a `PostCommentIntent` and
/// saves it to the repository for later processing by a reconciler.
async fn post_comment_handler(
    State(state): State<ApiState>,
    Json(intent): Json<PostCommentIntent>,
) -> impl IntoResponse {
    // 1. Verify the PoW challenge
    if !state.pow.verify(&intent.challenge_response) {
        return (StatusCode::FORBIDDEN, "Invalid Proof-of-Work response.").into_response();
    }

    // 2. Save the intent for the reconciler
    match state.storage.save_post_comment_intent(&intent).await {
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
/// It deserializes the user's request into a `DeleteCommentIntent` and
/// saves it to the repository for later processing by a reconciler.
async fn delete_comment_handler(
    State(state): State<ApiState>,
    Path(comment_id): Path<String>,
    Json(mut intent): Json<DeleteCommentIntent>,
) -> impl IntoResponse {
    // 1. Verify the PoW challenge
    if !state.pow.verify(&intent.challenge_response) {
        return (StatusCode::FORBIDDEN, "Invalid Proof-of-Work response.").into_response();
    }

    // 2. Ensure consistency between path and body
    if intent.event_id != comment_id {
        return (
            StatusCode::BAD_REQUEST,
            "Comment ID in path does not match ID in body.",
        )
            .into_response();
    }
    intent.event_id = comment_id;

    // 3. Save the intent for the reconciler
    match state.storage.save_delete_comment_intent(&intent).await {
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

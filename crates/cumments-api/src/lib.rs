use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use cumments_core::{
    intents::PostCommentIntent,
    models::Comment,
    ports::{CommentRepository, IntentRepository},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod pow;

// The shared state for our API.
#[derive(Clone)]
pub struct ApiState {
    pub storage: Arc<dyn CommentRepository + IntentRepository + Send + Sync>,
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

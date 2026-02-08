use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use cumments_core::{
    intents::PostCommentIntent,
    ports::{CommentRepository, IntentRepository},
};
use serde::Deserialize;
use std::sync::Arc;

// The shared state for our API.
// It holds a thread-safe reference to an object that implements all our storage ports.
#[derive(Clone)]
pub struct ApiState {
    pub storage: Arc<dyn CommentRepository + IntentRepository + Send + Sync>,
}

/// The query parameters for the `GET /api/comments` endpoint.
#[derive(Debug, Deserialize)]
pub struct GetCommentsQuery {
    pub site_id: String,
    pub post_slug: String,
}

/// Builds the Axum router for the API.
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route(
            "/api/comments",
            post(post_comment_handler).get(get_comments_handler),
        )
        .with_state(state)
}

/// The handler for fetching all comments for a given page.
async fn get_comments_handler(
    State(state): State<ApiState>,
    Query(query): Query<GetCommentsQuery>,
) -> impl IntoResponse {
    match state
        .storage
        .get_comments(&query.site_id, &query.post_slug)
        .await
    {
        Ok(comments) => (StatusCode::OK, Json(comments)).into_response(),
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

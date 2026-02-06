use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use cumments_core::{intents::PostCommentIntent, ports::IntentRepository};
use std::sync::Arc;

// The shared state for our API.
// It holds a thread-safe reference to the implementation of our storage port.
#[derive(Clone)]
pub struct ApiState {
    pub storage: Arc<dyn IntentRepository + Send + Sync>,
}

/// Builds the Axum router for the API.
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route("/api/comments", post(post_comment_handler))
        .with_state(state)
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
            (StatusCode::ACCEPTED, "Comment received and queued for processing.")
        }
        Err(e) => {
            tracing::error!("Failed to save comment intent: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to queue comment.")
        }
    }
}

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, post},
};
use cumments_core::{
    events::ProjectorEvent,
    intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent},
    models::{Comment, PostSlug, SiteId},
    ports::{CommentStore, IntentStore, SiteStore},
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Arc};
use tokio::sync::{Notify, broadcast};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use validator::Validate;

pub mod pow;

// --- Error Handling ---

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

pub enum AppError {
    InvalidPoW,
    Validation(validator::ValidationErrors),
    NotFound(String),
    Unauthorized(String),
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_msg, code, details) = match self {
            AppError::InvalidPoW => (
                StatusCode::FORBIDDEN,
                "Invalid Proof-of-Work response.".to_string(),
                "INVALID_POW",
                None,
            ),
            AppError::Validation(errs) => (
                StatusCode::BAD_REQUEST,
                "Input validation failed.".to_string(),
                "VALIDATION_ERROR",
                serde_json::to_value(errs).ok(),
            ),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, "NOT_FOUND", None),
            AppError::Unauthorized(msg) => (StatusCode::FORBIDDEN, msg, "UNAUTHORIZED", None),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg, "BAD_REQUEST", None),
            AppError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                msg,
                "INTERNAL_ERROR",
                None,
            ),
        };

        let body = Json(ErrorResponse {
            error: error_msg,
            code: code.to_string(),
            details,
        });

        (status, body).into_response()
    }
}

// ----------------------

// Define a new trait that combines the store traits for API use.
pub trait ApiStore: CommentStore + IntentStore + SiteStore + Send + Sync {}
impl<T: CommentStore + IntentStore + SiteStore + Send + Sync> ApiStore for T {}

// The shared state for our API.
#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<dyn ApiStore>,
    pub pow: Arc<pow::Pow>,
    pub event_bus: broadcast::Sender<ProjectorEvent>,
    pub reconciler_notify: Arc<Notify>,
}

/// The query parameters for pagination.
#[derive(Debug, Deserialize, Validate)]
pub struct PaginationQuery {
    #[validate(range(min = 1))]
    pub page: Option<i64>,
    #[validate(range(min = 1, max = 100))]
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
#[derive(Debug, Deserialize, Validate)]
pub struct PostCommentRequest {
    #[validate(length(min = 1, max = 5000))]
    pub content: String,
    #[validate(length(min = 1, max = 50))]
    pub nickname: String,
    #[validate(email)]
    pub email: Option<String>,
    #[validate(length(min = 8, max = 128))]
    pub author_fingerprint: String,
    pub reply_to: Option<String>,
    pub challenge_response: String,
}

/// Request DTO for deleting a comment.
#[derive(Debug, Deserialize, Validate)]
pub struct DeleteCommentRequest {
    #[validate(length(min = 8, max = 128))]
    pub author_fingerprint: String,
    pub challenge_response: String,
}

/// Request DTO for updating a comment.
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateCommentRequest {
    #[validate(length(min = 1, max = 5000))]
    pub content: String,
    #[validate(length(min = 8, max = 128))]
    pub author_fingerprint: String,
    pub challenge_response: String,
}

/// Builds the Axum router for the API.
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route(
            "/api/sites/{site_id}/posts/{post_slug}/comments",
            post(post_comment_handler).get(get_comments_handler),
        )
        .route(
            "/api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}",
            delete(delete_comment_handler).patch(update_comment_handler),
        )
        .route(
            "/api/sites/{site_id}/posts/{post_slug}/sse",
            get(sse_handler),
        )
        .route("/api/challenge", get(get_challenge_handler))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
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
    Path((site_id, post_slug)): Path<(String, String)>,
    Query(query): Query<PaginationQuery>,
) -> Result<impl IntoResponse, AppError> {
    // 1. Validate input
    query.validate().map_err(AppError::Validation)?;

    tracing::info!(
        "Fetching comments for site: {}, post: {}",
        site_id,
        post_slug
    );

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = per_page;
    let offset = (page - 1) * per_page;

    let site_id_val: SiteId = site_id.into();
    let post_slug_val: PostSlug = post_slug.into();

    match state
        .store
        .get_comments(&site_id_val, &post_slug_val, limit, offset)
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
            Ok((StatusCode::OK, Json(response)))
        }
        Err(e) => {
            tracing::error!("Failed to get comments: {:?}", e);
            Err(AppError::Internal(
                "Failed to retrieve comments.".to_string(),
            ))
        }
    }
}

/// The handler for receiving a new comment post.
async fn post_comment_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    Json(req): Json<PostCommentRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 3. Create the business intent
    let intent = PostCommentIntent {
        site_id: site_id.into(),
        post_slug: post_slug.into(),
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
            state.reconciler_notify.notify_one();
            Ok((
                StatusCode::ACCEPTED,
                "Comment received and queued for processing.",
            ))
        }

        Err(e) => {
            tracing::error!("Failed to save comment intent: {:?}", e);
            Err(AppError::Internal("Failed to queue comment.".to_string()))
        }
    }
}

/// The handler for receiving a new delete comment request.
async fn delete_comment_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug, comment_id)): Path<(String, String, String)>,
    Json(req): Json<DeleteCommentRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 2. Authorization: Verify that the fingerprint matches the original author
    match state.store.get_comment(&comment_id).await {
        Ok(Some(comment)) => {
            if let Some(original_fp) = comment.author_fingerprint {
                if original_fp != req.author_fingerprint {
                    return Err(AppError::Unauthorized(
                        "You are not authorized to delete this comment.".to_string(),
                    ));
                }
            } else {
                return Err(AppError::Unauthorized(
                    "Cannot verify ownership for this comment.".to_string(),
                ));
            }
        }
        Ok(None) => {
            return Err(AppError::NotFound("Comment not found.".to_string()));
        }
        Err(e) => {
            tracing::error!("Failed to fetch comment for authorization: {:?}", e);
            return Err(AppError::Internal(
                "Internal server error during authorization.".to_string(),
            ));
        }
    }

    // 3. Create the business intent
    let intent = DeleteCommentIntent {
        site_id: site_id.into(),
        post_slug: post_slug.into(),
        event_id: comment_id,
        author_fingerprint: req.author_fingerprint,
    };

    // 4. Save the intent for the reconciler
    match state.store.save_delete_intent(&intent).await {
        Ok(_) => {
            tracing::info!("Successfully saved a delete comment intent.");
            state.reconciler_notify.notify_one();
            Ok((
                StatusCode::ACCEPTED,
                "Delete request received and queued for processing.",
            ))
        }
        Err(e) => {
            tracing::error!("Failed to save delete comment intent: {:?}", e);
            Err(AppError::Internal(
                "Failed to queue delete request.".to_string(),
            ))
        }
    }
}

/// The handler for receiving a new update comment request.
async fn update_comment_handler(
    State(state): State<ApiState>,
    Path((_site_id, _post_slug, comment_id)): Path<(String, String, String)>,
    Json(req): Json<UpdateCommentRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 3. Authorization: Verify that the fingerprint matches the original author
    match state.store.get_comment(&comment_id).await {
        Ok(Some(comment)) => {
            if let Some(original_fp) = comment.author_fingerprint {
                if original_fp != req.author_fingerprint {
                    return Err(AppError::Unauthorized(
                        "You are not authorized to edit this comment.".to_string(),
                    ));
                }
            } else {
                return Err(AppError::Unauthorized(
                    "Cannot verify ownership for this comment.".to_string(),
                ));
            }
        }
        Ok(None) => {
            return Err(AppError::NotFound("Comment not found.".to_string()));
        }
        Err(e) => {
            tracing::error!("Failed to fetch comment for authorization: {:?}", e);
            return Err(AppError::Internal(
                "Internal server error during authorization.".to_string(),
            ));
        }
    }

    // 4. Create the business intent
    let intent = UpdateCommentIntent {
        site_id: _site_id.into(),
        post_slug: _post_slug.into(),
        event_id: comment_id,
        content: req.content,
        author_fingerprint: req.author_fingerprint,
    };

    // 5. Save the intent for the reconciler
    match state.store.save_update_intent(&intent).await {
        Ok(_) => {
            tracing::info!("Successfully saved an update comment intent.");
            state.reconciler_notify.notify_one();
            Ok((
                StatusCode::ACCEPTED,
                "Update request received and queued for processing.",
            ))
        }
        Err(e) => {
            tracing::error!("Failed to save update comment intent: {:?}", e);
            Err(AppError::Internal(
                "Failed to queue update request.".to_string(),
            ))
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

            if matches && let Ok(json) = serde_json::to_string(&event) {
                yield Ok(Event::default().data(json));
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderName, HeaderValue, Method, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, post},
};
use cumments_core::{
    events::ProjectorEvent,
    identity::{signature_message, verify_signature},
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
    InvalidSignature,
    Validation(validator::ValidationErrors),
    NotFound(String),
    MethodNotAllowed,
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
            AppError::InvalidSignature => (
                StatusCode::FORBIDDEN,
                "Invalid author signature.".to_string(),
                "INVALID_SIGNATURE",
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
            AppError::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                "Method not allowed. Use QUERY for queries, POST for submissions.".to_string(),
                "METHOD_NOT_ALLOWED",
                None,
            ),
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

/// The query parameters for pagination (sent as JSON body for QUERY method).
#[derive(Debug, Deserialize, Validate)]
pub struct PaginationQuery {
    // The upper bound keeps `(page - 1) * per_page` inside i64 even with the
    // largest allowed per_page (100).
    #[validate(range(min = 1, max = 1_000_000))]
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
    /// Ed25519 public key of the author (base64url, 32 bytes raw).
    pub author_public_key: String,
    /// Ed25519 signature over the canonical POST message.
    pub author_signature: String,
    pub reply_to: Option<String>,
    pub challenge_response: String,
}

/// Request DTO for deleting a comment.
#[derive(Debug, Deserialize, Validate)]
pub struct DeleteCommentRequest {
    pub author_public_key: String,
    pub author_signature: String,
    pub challenge_response: String,
}

/// Request DTO for updating a comment.
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateCommentRequest {
    #[validate(length(min = 1, max = 5000))]
    pub content: String,
    pub author_public_key: String,
    pub author_signature: String,
    pub challenge_response: String,
}

/// The QUERY method for HTTP.
static QUERY_METHOD: std::sync::LazyLock<Method> =
    std::sync::LazyLock::new(|| Method::from_bytes(b"QUERY").unwrap());

/// The Accept-Query response header (RFC 10008, Section 3).
static ACCEPT_QUERY: std::sync::LazyLock<HeaderName> =
    std::sync::LazyLock::new(|| HeaderName::from_static("accept-query"));

/// Build the CORS layer from a comma-separated origin list.
///
/// `"*"` keeps permissive behavior; an empty list disables cross-origin
/// support (no CORS headers are sent); any other value restricts
/// `Access-Control-Allow-Origin` to the listed origins and explicitly allows
/// the methods and headers used by the API (including the custom `QUERY`
/// method).
fn cors_layer(cors_origins: &str) -> Option<CorsLayer> {
    let origins: Vec<&str> = cors_origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if origins.is_empty() {
        return None;
    }
    if origins.contains(&"*") {
        return Some(CorsLayer::permissive());
    }

    let allowed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    Some(
        CorsLayer::new()
            .allow_origin(allowed)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PATCH,
                Method::DELETE,
                (*QUERY_METHOD).clone(),
            ])
            .allow_headers([HeaderName::from_static("content-type")]),
    )
}

/// Extract the signed challenge prefix from a `challenge|nonce` response.
fn challenge_prefix(challenge_response: &str) -> &str {
    challenge_response.split('|').next().unwrap_or("")
}

/// Builds the Axum router for the API.
pub fn build_router(state: ApiState, cors_origins: &str) -> Router {
    let router = Router::new()
        .route(
            "/api/sites/{site_id}/posts/{post_slug}/comments",
            // POST for writing intents, fallback handles QUERY for reading.
            post(post_comment_handler).fallback(query_comments_handler),
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
        .route("/health", get(health_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    match cors_layer(cors_origins) {
        Some(cors) => router.layer(cors),
        None => router,
    }
}

/// Simple liveness endpoint used by container healthchecks.
async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
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

/// The handler for querying comments via the QUERY method (RFC 10008).
///
/// Pagination parameters are passed as JSON request body instead of
/// URL query string, allowing for future extension to complex filters.
///
/// This handler is registered as the fallback for non-POST requests.
/// It validates the method and rejects non-QUERY requests with 405.
async fn query_comments_handler(
    method: Method,
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    // 1. Only QUERY method is accepted here
    if method != *QUERY_METHOD {
        return Err(AppError::MethodNotAllowed);
    }

    // 2. Validate path params
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(post_slug).map_err(AppError::Validation)?;

    // 3. Parse pagination from JSON body (empty body → defaults)
    let query: PaginationQuery = if body.is_empty() {
        PaginationQuery {
            page: None,
            per_page: None,
        }
    } else {
        serde_json::from_str(&body)
            .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?
    };
    query.validate().map_err(AppError::Validation)?;

    tracing::info!(
        "QUERY comments for site: {}, post: {} (page: {:?}, per_page: {:?})",
        site_id_val.as_str(),
        post_slug_val.as_str(),
        query.page,
        query.per_page,
    );

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = per_page;
    let offset = (page - 1) * per_page;

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

            // Advertise QUERY support and accepted media type per RFC 10008.
            let headers = [(ACCEPT_QUERY.clone(), "application/json")];
            Ok((headers, (StatusCode::OK, Json(response))))
        }
        Err(e) => {
            tracing::error!("Failed to query comments: {:?}", e);
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

    // 2b. Verify the author's Ed25519 signature over the canonical message.
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        "POST",
        &site_id,
        &post_slug,
        &req.content,
        &req.nickname,
        challenge,
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    // 3. Create the business intent
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(post_slug).map_err(AppError::Validation)?;

    let intent = PostCommentIntent {
        site_id: site_id_val,
        post_slug: post_slug_val,
        content: req.content,
        nickname: req.nickname,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
        reply_to: req.reply_to,
    };

    // 4. Save the intent for the reconciler
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

    // 2b. Verify the author's Ed25519 signature.
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&["DELETE", &site_id, &post_slug, &comment_id, challenge]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    // 2c. Authorization: the presented public key must be the comment's owner.
    match state.store.get_comment(&comment_id).await {
        Ok(Some(comment)) => {
            if comment.site_id != site_id || comment.post_slug != post_slug {
                return Err(AppError::NotFound("Comment not found.".to_string()));
            }
            match state.store.get_comment_author_public_key(&comment_id).await {
                Ok(Some(expected)) if expected == req.author_public_key => {}
                Ok(Some(_)) => {
                    return Err(AppError::Unauthorized(
                        "You are not authorized to delete this comment.".to_string(),
                    ));
                }
                Ok(None) => {
                    return Err(AppError::Unauthorized(
                        "Cannot verify ownership for this comment.".to_string(),
                    ));
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to fetch comment owner verifier for authorization: {:?}",
                        e
                    );
                    return Err(AppError::Internal(
                        "Internal server error during authorization.".to_string(),
                    ));
                }
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
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(post_slug).map_err(AppError::Validation)?;
    let intent = DeleteCommentIntent {
        site_id: site_id_val,
        post_slug: post_slug_val,
        event_id: comment_id,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
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
    Path((site_id, post_slug, comment_id)): Path<(String, String, String)>,
    Json(req): Json<UpdateCommentRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 3b. Verify the author's Ed25519 signature.
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        "PATCH",
        &site_id,
        &post_slug,
        &comment_id,
        &req.content,
        challenge,
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    // 3c. Authorization: the presented public key must be the comment's owner,
    // and the comment must belong to the site/post in the path.
    match state.store.get_comment(&comment_id).await {
        Ok(Some(comment)) => {
            if comment.site_id != site_id || comment.post_slug != post_slug {
                return Err(AppError::NotFound("Comment not found.".to_string()));
            }
            match state.store.get_comment_author_public_key(&comment_id).await {
                Ok(Some(expected)) if expected == req.author_public_key => {}
                Ok(Some(_)) => {
                    return Err(AppError::Unauthorized(
                        "You are not authorized to edit this comment.".to_string(),
                    ));
                }
                Ok(None) => {
                    return Err(AppError::Unauthorized(
                        "Cannot verify ownership for this comment.".to_string(),
                    ));
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to fetch comment owner verifier for authorization: {:?}",
                        e
                    );
                    return Err(AppError::Internal(
                        "Internal server error during authorization.".to_string(),
                    ));
                }
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
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(post_slug).map_err(AppError::Validation)?;
    let intent = UpdateCommentIntent {
        site_id: site_id_val,
        post_slug: post_slug_val,
        event_id: comment_id,
        content: req.content,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
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
                let event_name = match &event {
                    ProjectorEvent::NewComment { .. } => "new_comment",
                    ProjectorEvent::CommentUpdated { .. } => "comment_updated",
                    ProjectorEvent::CommentDeleted { .. } => "comment_deleted",
                };
                yield Ok(Event::default().event(event_name).data(json));
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use validator::Validate;

    #[test]
    fn pagination_page_is_bounded() {
        let ok = PaginationQuery {
            page: Some(1_000_000),
            per_page: Some(100),
        };
        assert!(ok.validate().is_ok());

        let too_large = PaginationQuery {
            page: Some(i64::MAX),
            per_page: Some(100),
        };
        assert!(too_large.validate().is_err());

        let zero = PaginationQuery {
            page: Some(0),
            per_page: None,
        };
        assert!(zero.validate().is_err());
    }

    #[test]
    fn empty_cors_list_disables_cross_origin_layer() {
        assert!(cors_layer("").is_none());
        assert!(cors_layer("  , , ").is_none());
        assert!(cors_layer("*").is_some());
        assert!(cors_layer("https://blog.example.com").is_some());
    }
}

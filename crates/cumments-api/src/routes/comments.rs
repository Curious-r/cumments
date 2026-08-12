//! Comment query/post/delete/update route handlers.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use crate::request::{
    DeleteCommentRequest, PaginatedResponse, PaginationMeta, PaginationQuery, PostCommentRequest,
    UpdateCommentRequest,
};
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use cumments_core::{
    identity::{post_signature_message, signature_message, verify_signature},
    intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent},
    models::{AuthorKind, PostSlug, SiteId},
    ports::{IdempotencyInput, IdempotencyOutcome},
    site_auth::sha256_hex,
};
use ruma_common::EventId;
use validator::Validate;

pub(crate) static QUERY_METHOD: std::sync::LazyLock<Method> =
    std::sync::LazyLock::new(|| Method::from_bytes(b"QUERY").unwrap());

/// The Accept-Query response header (RFC 10008, Section 3).
pub(crate) static ACCEPT_QUERY: std::sync::LazyLock<HeaderName> =
    std::sync::LazyLock::new(|| HeaderName::from_static("accept-query"));

/// Mandatory `Idempotency-Key` header for POST/PATCH/DELETE intents.
static IDEMPOTENCY_KEY_HEADER: std::sync::LazyLock<HeaderName> =
    std::sync::LazyLock::new(|| HeaderName::from_static("idempotency-key"));

/// Response header marking a replay of an already-accepted request.
static IDEMPOTENT_REPLAYED: std::sync::LazyLock<HeaderName> =
    std::sync::LazyLock::new(|| HeaderName::from_static("idempotent-replayed"));

/// Build the CORS layer from a comma-separated origin list.
fn challenge_prefix(challenge_response: &str) -> &str {
    challenge_response.split('|').next().unwrap_or("")
}

/// Error returned when `reply_to` cannot be a Matrix event ID.
const REPLY_TO_FORMAT_ERROR: &str =
    "reply_to must be a Matrix event ID (e.g. \"$event\" or \"$event:server\")";
const COMMENT_ID_FORMAT_ERROR: &str =
    "comment_id must be a Matrix event ID (e.g. \"$event\" or \"$event:server\")";

/// Basic shape check for a Matrix event ID used as a reply target.
///
/// Event IDs are opaque by spec, but their shape depends on the room version:
/// v1/v2 use `$opaque_id:server_name`, v3 uses a bare unpadded-Base64 hash
/// (which may contain `/` or `+`), and v4+ uses URL-safe unpadded Base64. We
/// parse with ruma, the reference Matrix identifier implementation, then
/// enforce the invariants every room version shares: a non-empty localpart
/// and at most 255 bytes.
fn validate_reply_to_format(reply_to: &str) -> Result<(), &'static str> {
    let event_id = EventId::parse(reply_to).map_err(|_| REPLY_TO_FORMAT_ERROR)?;
    if event_id.localpart().is_empty() || reply_to.len() > 255 {
        return Err(REPLY_TO_FORMAT_ERROR);
    }
    Ok(())
}

/// Same shape check for comment IDs used in PATCH/DELETE paths.
fn validate_comment_id_format(comment_id: &str) -> Result<(), &'static str> {
    let event_id = EventId::parse(comment_id).map_err(|_| COMMENT_ID_FORMAT_ERROR)?;
    if event_id.localpart().is_empty() || comment_id.len() > 255 {
        return Err(COMMENT_ID_FORMAT_ERROR);
    }
    Ok(())
}

/// Reads and validates the mandatory `Idempotency-Key` header.
///
/// Keys are 8-255 printable ASCII characters. Validation failures return a
/// 400 and never record the key, so the same key can be retried with a valid
/// request.
fn extract_idempotency_key(headers: &HeaderMap) -> Result<String, AppError> {
    let value = headers.get(&*IDEMPOTENCY_KEY_HEADER).ok_or_else(|| {
        AppError::IdempotencyKeyRequired(
            "Idempotency-Key header is required for write requests.".to_string(),
        )
    })?;
    let value = value.to_str().map_err(|_| {
        AppError::InvalidIdempotencyKey(
            "Idempotency-Key must contain only printable ASCII characters.".to_string(),
        )
    })?;
    if !(8..=255).contains(&value.len()) {
        return Err(AppError::InvalidIdempotencyKey(
            "Idempotency-Key must be 8-255 characters long.".to_string(),
        ));
    }
    if !value.bytes().all(|b| (0x21..=0x7E).contains(&b)) {
        return Err(AppError::InvalidIdempotencyKey(
            "Idempotency-Key must contain only printable ASCII characters ".to_string()
                + "(no spaces or control characters).",
        ));
    }
    Ok(value.to_owned())
}

/// Canonical fingerprint of one write request.
///
/// `METHOD\npath\nsha256(body)` — the body is hashed first so the fingerprint
/// stays compact for large payloads. The path is reconstructed from the
/// validated route parameters rather than the raw URL, so equivalent
/// percent-encoding choices still produce the same fingerprint.
fn request_fingerprint(method: &str, path: &str, body: &str) -> String {
    format!("{}\n{}\n{}", method, path, sha256_hex(body.as_bytes()))
}

/// Builds the `202 { intent_id }` response, marking replays explicitly.
fn accepted_response(intent_id: i64, replayed: bool) -> Response {
    let mut response = (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "intent_id": intent_id })),
    )
        .into_response();
    if replayed {
        response.headers_mut().insert(
            IDEMPOTENT_REPLAYED.clone(),
            HeaderValue::from_static("true"),
        );
    }
    response
}

/// Route parameters shared by the PATCH/DELETE handlers, plus the canonical
/// fingerprint path for the endpoint form actually used.
struct CommentWritePath {
    site_id: String,
    post_slug: String,
    comment_id: String,
    fingerprint_path: String,
}

impl CommentWritePath {
    fn new(site_id: String, post_slug: String, comment_id: String, path_form: bool) -> Self {
        let fingerprint_path = if path_form {
            format!("/api/v1/sites/{site_id}/posts/{post_slug}/comments/{comment_id}")
        } else {
            format!("/api/v1/sites/{site_id}/posts/{post_slug}/comments")
        };
        Self {
            site_id,
            post_slug,
            comment_id,
            fingerprint_path,
        }
    }
}

pub(crate) async fn query_comments_handler(
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
        .get_messages(&site_id_val, &post_slug_val, limit, offset)
        .await
    {
        Ok(mut page_data) => {
            if let Some(proxy) = &state.media_proxy {
                for message in &mut page_data.items {
                    proxy.proxify_message(message);
                }
            }
            let total = page_data.total;
            let total_pages = if total > 0 {
                (total + per_page - 1) / per_page
            } else {
                0
            };

            let response = PaginatedResponse {
                data: page_data.items,
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
pub(crate) async fn post_comment_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(AppError::TooManyRequests(
            "comment writes are rate limited; try again later".to_string(),
        ));
    }

    let idempotency_key = extract_idempotency_key(&headers)?;
    let req: PostCommentRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let fingerprint = request_fingerprint(
        "POST",
        &format!("/api/v1/sites/{}/posts/{}/comments", site_id, post_slug),
        &body,
    );

    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;
    if req
        .reply_to
        .as_deref()
        .is_some_and(|reply_to| reply_to.trim().is_empty())
    {
        return Err(AppError::BadRequest(
            "reply_to must not be empty when provided.".to_string(),
        ));
    }
    if let Some(reply_to) = req.reply_to.as_deref()
        && let Err(msg) = validate_reply_to_format(reply_to)
    {
        return Err(AppError::BadRequest(msg.to_string()));
    }

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 2b. Verify the author's Ed25519 signature over the canonical message.
    let challenge = challenge_prefix(&req.challenge_response);
    let message = post_signature_message(
        &site_id,
        &post_slug,
        &req.content,
        &req.display_name,
        req.reply_to.as_deref(),
        challenge,
    );
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    // The reply target must belong to the same site/post when it is already
    // visible in the read model. Unknown targets are accepted so a fast reply
    // does not depend on projection timing; Matrix relation semantics still
    // apply.
    if let Some(reply_to) = req.reply_to.as_deref() {
        match state.store.get_message(reply_to).await {
            Ok(Some(parent)) => {
                if parent.site_id != site_id || parent.post_slug != post_slug {
                    return Err(AppError::BadRequest(
                        "reply_to must reference a comment in the same site and post.".to_string(),
                    ));
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("Failed to validate reply target: {:?}", e);
                return Err(AppError::Internal(
                    "Failed to validate reply target.".to_string(),
                ));
            }
        }
    }

    // 3. Create the business intent
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(post_slug).map_err(AppError::Validation)?;

    let intent = PostCommentIntent {
        site_id: site_id_val,
        post_slug: post_slug_val,
        content: req.content,
        display_name: req.display_name,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
        reply_to: req.reply_to,
    };

    // 4. Save the intent for the reconciler, atomically with its idempotency
    // record so retries can never queue duplicate work.
    match state
        .store
        .save_post_intent_idempotent(
            &intent,
            &IdempotencyInput {
                author_public_key: intent.author_public_key.clone(),
                key: idempotency_key,
                request_fingerprint: fingerprint,
            },
        )
        .await
    {
        Ok(IdempotencyOutcome::Accepted { intent_id }) => {
            tracing::info!("Successfully saved a new comment intent.");
            state.reconciler_notify.notify_one();
            Ok(accepted_response(intent_id, false))
        }
        Ok(IdempotencyOutcome::Replayed { intent_id }) => {
            tracing::info!("Replayed idempotent POST with intent_id {}", intent_id);
            Ok(accepted_response(intent_id, true))
        }
        Ok(IdempotencyOutcome::Reused) => Err(AppError::IdempotencyReused),
        Err(e) => {
            tracing::error!("Failed to save comment intent: {:?}", e);
            Err(AppError::Internal("Failed to queue comment.".to_string()))
        }
    }
}

/// The handler for receiving a new delete comment request.
pub(crate) async fn delete_comment_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug, comment_id)): Path<(String, String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let req: DeleteCommentRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let path = CommentWritePath::new(site_id, post_slug, comment_id, true);
    delete_comment_common(state, connect, headers, path, req, body).await
}

/// Delete via the collection endpoint, with `comment_id` in the JSON body so
/// opaque Matrix event IDs never need percent-encoding in the URL.
pub(crate) async fn delete_comment_body_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let req: DeleteCommentRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let comment_id = req
        .comment_id
        .clone()
        .ok_or_else(|| AppError::BadRequest("comment_id is required".to_string()))?;
    let path = CommentWritePath::new(site_id, post_slug, comment_id, false);
    delete_comment_common(state, connect, headers, path, req, body).await
}

async fn delete_comment_common(
    state: ApiState,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    path: CommentWritePath,
    req: DeleteCommentRequest,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(AppError::TooManyRequests(
            "comment writes are rate limited; try again later".to_string(),
        ));
    }

    let idempotency_key = extract_idempotency_key(&headers)?;
    let fingerprint = request_fingerprint("DELETE", &path.fingerprint_path, &body);

    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;
    if let Err(msg) = validate_comment_id_format(&path.comment_id) {
        return Err(AppError::BadRequest(msg.to_string()));
    }

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 2b. Verify the author's Ed25519 signature.
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        "DELETE",
        &path.site_id,
        &path.post_slug,
        &path.comment_id,
        challenge,
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    // 2c. Authorization: the presented public key must be the comment's owner.
    match state.store.get_message(&path.comment_id).await {
        Ok(Some(message)) => {
            if message.site_id != path.site_id || message.post_slug != path.post_slug {
                return Err(AppError::NotFound("Comment not found.".to_string()));
            }
            if message.author.kind == AuthorKind::Matrix {
                return Err(AppError::NotManageable(
                    "This comment was posted by a Matrix user; manage it from a Matrix client."
                        .to_string(),
                ));
            }
            match state.store.get_author_public_key(&path.comment_id).await {
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
    let site_id_val = SiteId::new(path.site_id).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(path.post_slug).map_err(AppError::Validation)?;
    let intent = DeleteCommentIntent {
        site_id: site_id_val,
        post_slug: post_slug_val,
        event_id: path.comment_id,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
    };

    // 4. Save the intent for the reconciler, atomically with its idempotency
    // record so retries can never queue duplicate work.
    match state
        .store
        .save_delete_intent_idempotent(
            &intent,
            &IdempotencyInput {
                author_public_key: intent.author_public_key.clone(),
                key: idempotency_key,
                request_fingerprint: fingerprint,
            },
        )
        .await
    {
        Ok(IdempotencyOutcome::Accepted { intent_id }) => {
            tracing::info!("Successfully saved a delete comment intent.");
            state.reconciler_notify.notify_one();
            Ok(accepted_response(intent_id, false))
        }
        Ok(IdempotencyOutcome::Replayed { intent_id }) => {
            tracing::info!("Replayed idempotent DELETE with intent_id {}", intent_id);
            Ok(accepted_response(intent_id, true))
        }
        Ok(IdempotencyOutcome::Reused) => Err(AppError::IdempotencyReused),
        Err(e) => {
            tracing::error!("Failed to save delete comment intent: {:?}", e);
            Err(AppError::Internal(
                "Failed to queue delete request.".to_string(),
            ))
        }
    }
}

/// The handler for receiving a new update comment request.
pub(crate) async fn update_comment_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug, comment_id)): Path<(String, String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let req: UpdateCommentRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let path = CommentWritePath::new(site_id, post_slug, comment_id, true);
    update_comment_common(state, connect, headers, path, req, body).await
}

/// Edit via the collection endpoint, with `comment_id` in the JSON body so
/// opaque Matrix event IDs never need percent-encoding in the URL.
pub(crate) async fn update_comment_body_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let req: UpdateCommentRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let comment_id = req
        .comment_id
        .clone()
        .ok_or_else(|| AppError::BadRequest("comment_id is required".to_string()))?;
    let path = CommentWritePath::new(site_id, post_slug, comment_id, false);
    update_comment_common(state, connect, headers, path, req, body).await
}

async fn update_comment_common(
    state: ApiState,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    path: CommentWritePath,
    req: UpdateCommentRequest,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(AppError::TooManyRequests(
            "comment writes are rate limited; try again later".to_string(),
        ));
    }

    let idempotency_key = extract_idempotency_key(&headers)?;
    let fingerprint = request_fingerprint("PATCH", &path.fingerprint_path, &body);

    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;
    if let Err(msg) = validate_comment_id_format(&path.comment_id) {
        return Err(AppError::BadRequest(msg.to_string()));
    }

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 3b. Verify the author's Ed25519 signature.
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        "PATCH",
        &path.site_id,
        &path.post_slug,
        &path.comment_id,
        &req.content,
        challenge,
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    // 3c. Authorization: the presented public key must be the comment's owner,
    // and the comment must belong to the site/post in the path.
    match state.store.get_message(&path.comment_id).await {
        Ok(Some(message)) => {
            if message.site_id != path.site_id || message.post_slug != path.post_slug {
                return Err(AppError::NotFound("Comment not found.".to_string()));
            }
            if message.author.kind == AuthorKind::Matrix {
                return Err(AppError::NotManageable(
                    "This comment was posted by a Matrix user; manage it from a Matrix client."
                        .to_string(),
                ));
            }
            match state.store.get_author_public_key(&path.comment_id).await {
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
    let site_id_val = SiteId::new(path.site_id).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(path.post_slug).map_err(AppError::Validation)?;
    let intent = UpdateCommentIntent {
        site_id: site_id_val,
        post_slug: post_slug_val,
        event_id: path.comment_id,
        content: req.content,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
    };

    // 5. Save the intent for the reconciler, atomically with its idempotency
    // record so retries can never queue duplicate work.
    match state
        .store
        .save_update_intent_idempotent(
            &intent,
            &IdempotencyInput {
                author_public_key: intent.author_public_key.clone(),
                key: idempotency_key,
                request_fingerprint: fingerprint,
            },
        )
        .await
    {
        Ok(IdempotencyOutcome::Accepted { intent_id }) => {
            tracing::info!("Successfully saved an update comment intent.");
            state.reconciler_notify.notify_one();
            Ok(accepted_response(intent_id, false))
        }
        Ok(IdempotencyOutcome::Replayed { intent_id }) => {
            tracing::info!("Replayed idempotent PATCH with intent_id {}", intent_id);
            Ok(accepted_response(intent_id, true))
        }
        Ok(IdempotencyOutcome::Reused) => Err(AppError::IdempotencyReused),
        Err(e) => {
            tracing::error!("Failed to save update comment intent: {:?}", e);
            Err(AppError::Internal(
                "Failed to queue update request.".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_to_format_validation_accepts_event_ids_and_rejects_garbage() {
        // Legacy room v1/v2 form (localpart:server).
        assert!(validate_reply_to_format("$event:server").is_ok());
        assert!(validate_reply_to_format("$a:hs").is_ok());
        // Room v3 form: unpadded Base64 hash (may contain `/`).
        assert!(validate_reply_to_format("$acR1l0raoZnm60CBwAVgqbZqoO/mYU81xysh1u7XcJk").is_ok());
        // Room v4+ form: URL-safe unpadded Base64 hash.
        assert!(validate_reply_to_format("$Rqnc-F-dvnEYJTyHq_iKxU2bZ1CI92-kuZq3a5lr5Zg").is_ok());
        // A real-world tuwunel event ID.
        assert!(validate_reply_to_format("$rCeBvRcif7pRbHMIiPRWcA3m5kKpbLg1p7qfWp73lhM").is_ok());
        assert!(validate_reply_to_format("not-an-event").is_err());
        assert!(validate_reply_to_format("$").is_err());
        assert!(validate_reply_to_format("$:server").is_err());
        assert!(validate_reply_to_format("$x:bad server").is_err());
        assert!(validate_reply_to_format(&format!("${}", "x".repeat(300))).is_err());
    }

    #[test]
    fn idempotency_key_validation_enforces_length_and_printable_ascii() {
        let mut headers = HeaderMap::new();

        // Missing key.
        assert!(extract_idempotency_key(&headers).is_err());

        // Too short.
        headers.insert(
            IDEMPOTENCY_KEY_HEADER.clone(),
            HeaderValue::from_static("short"),
        );
        assert!(extract_idempotency_key(&headers).is_err());

        // Non-printable / space characters.
        headers.insert(
            IDEMPOTENCY_KEY_HEADER.clone(),
            HeaderValue::from_static("has space 123"),
        );
        assert!(extract_idempotency_key(&headers).is_err());

        // Valid printable ASCII key.
        headers.insert(
            IDEMPOTENCY_KEY_HEADER.clone(),
            HeaderValue::from_static("valid-key-123456"),
        );
        let key = extract_idempotency_key(&headers);
        assert!(key.is_ok());
        assert_eq!(key.unwrap_or_default(), "valid-key-123456");
    }

    #[test]
    fn request_fingerprint_is_stable_and_sensitive_to_method_and_body() {
        let first = request_fingerprint("POST", "/api/v1/posts/p", "{}");
        assert_eq!(first, request_fingerprint("POST", "/api/v1/posts/p", "{}"));
        assert_ne!(first, request_fingerprint("POST", "/api/v1/posts/p", "{ }"));
        assert_ne!(first, request_fingerprint("PATCH", "/api/v1/posts/p", "{}"));
        assert_ne!(
            first,
            request_fingerprint("POST", "/api/v1/posts/other", "{}")
        );
    }

    #[test]
    fn accepted_response_marks_replays() {
        let response = accepted_response(42, true);
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            response
                .headers()
                .get(&*IDEMPOTENT_REPLAYED)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );

        let response = accepted_response(42, false);
        assert!(
            response.headers().get(&*IDEMPOTENT_REPLAYED).is_none(),
            "fresh accepts must not carry the replay marker"
        );
    }
}

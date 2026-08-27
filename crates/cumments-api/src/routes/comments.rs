//! Comment query/post/delete/update route handlers.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use crate::request::{
    DeleteCommentRequest, IDEMPOTENT_REPLAYED, LocationRequest, PaginatedResponse, PaginationMeta,
    PaginationQuery, PostCommentRequest, ReactRequest, UnreactRequest, UpdateCommentRequest,
    VoteRequest, extract_idempotency_key, request_fingerprint,
};
use crate::routes::media::media_url_base;
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use cumments_core::{
    commands::{DeleteCommentCommand, LocationPayload, PostCommentCommand, UpdateCommentCommand},
    identity::{
        derive_visitor_id_from_public_key, post_signature_message, signature_message,
        verify_signature,
    },
    models::{AuthorKind, Content, MediaKind, Message, MessageStatus, PageSlug, SiteId},
    submissions::{IdempotencyInput, IdempotencyOutcome, deterministic_transaction_id},
};
use ruma_common::EventId;
use std::net::SocketAddr;
use validator::Validate;

pub(crate) static QUERY_METHOD: std::sync::LazyLock<Method> =
    std::sync::LazyLock::new(|| Method::from_bytes(b"QUERY").unwrap());

/// The Accept-Query response header (RFC 10008, Section 3).
pub(crate) static ACCEPT_QUERY: std::sync::LazyLock<HeaderName> =
    std::sync::LazyLock::new(|| HeaderName::from_static("accept-query"));

/// Build the CORS layer from a comma-separated origin list.
pub(crate) fn challenge_prefix(challenge_response: &str) -> &str {
    challenge_response.split('|').next().unwrap_or("")
}

/// Normalize a reaction key (emoji) for storage and lookup.
/// Trims surrounding whitespace, rejects empty, control characters and overly long keys.
fn normalize_reaction_key(key: &str) -> Result<String, AppError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(
            "reaction key must not be empty or whitespace".to_string(),
        ));
    }
    if trimmed != key {
        return Err(AppError::BadRequest(
            "reaction key must not have leading or trailing whitespace".to_string(),
        ));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(AppError::BadRequest(
            "reaction key must not contain control characters".to_string(),
        ));
    }
    // Length check is already enforced by validator, but keep a defensive bound.
    if trimmed.len() > 32 {
        return Err(AppError::BadRequest(
            "reaction key must be at most 32 bytes".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// 429 error for comment-write rate limiting, using the write limiter's
/// fixed window as the conservative `Retry-After` value.
fn comment_write_rate_limited(state: &ApiState) -> AppError {
    AppError::TooManyRequests {
        detail: "comment writes are rate limited; try again later".to_string(),
        retry_after_seconds: state.write_limiter.window().as_secs(),
    }
}

/// Error returned when `reply_to` cannot be a Matrix event ID.
const REPLY_TO_FORMAT_ERROR: &str =
    "reply_to must be a Matrix event ID (e.g. \"$event\" or \"$event:server\")";
const COMMENT_ID_FORMAT_ERROR: &str =
    "comment_id must be a Matrix event ID (e.g. \"$event\" or \"$event:server\")";

/// Rejects a target that belongs to another page or is already a tombstone.
fn active_target_in_page(
    message: &Message,
    site_id: &str,
    page_slug: &str,
) -> Result<(), AppError> {
    if message.site_id != site_id || message.page_slug != page_slug {
        return Err(AppError::NotFound("Comment not found.".to_string()));
    }
    if message.status != MessageStatus::Active {
        return Err(AppError::Conflict(
            "The target comment has been deleted.".to_string(),
        ));
    }
    Ok(())
}

/// Loose MSC3488 / RFC 5870 shape check: `geo:lat,lon` with optional
/// `;key=value` parameters, where the coordinates are decimal numbers in
/// range.
fn is_valid_geo_uri(geo_uri: &str) -> bool {
    let Some(rest) = geo_uri.strip_prefix("geo:") else {
        return false;
    };
    let coords = rest.split_once(';').map_or(rest, |(coords, _)| coords);
    let Some((lat, lon)) = coords.split_once(',') else {
        return false;
    };
    let Ok(lat) = lat.parse::<f64>() else {
        return false;
    };
    let Ok(lon) = lon.parse::<f64>() else {
        return false;
    };
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

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

const THREAD_ROOT_FORMAT_ERROR: &str =
    "thread_root must be a Matrix event ID (e.g. \"$event\" or \"$event:server\")";

fn validate_thread_root_format(thread_root: &str) -> Result<(), &'static str> {
    validate_reply_to_format(thread_root).map_err(|_| THREAD_ROOT_FORMAT_ERROR)
}

/// Builds the `202 { submission_id }` response, marking replays explicitly.
fn accepted_response(submission_id: i64, replayed: bool) -> Response {
    let mut response = (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "submission_id": submission_id })),
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

/// Short-circuits an already-accepted idempotency key before PoW/signature
/// verification: a replay returns the original submission ID (and the replay
/// marker) without consuming a fresh PoW challenge, while reusing the key
/// with a different request is rejected.
async fn idempotency_short_circuit(
    state: &ApiState,
    idempotency: &IdempotencyInput,
) -> Result<Option<Response>, AppError> {
    match state.store.lookup_idempotency(idempotency).await {
        Ok(Some(IdempotencyOutcome::Replayed { submission_id })) => {
            tracing::info!(
                "Replayed idempotent request for submission_id {}",
                submission_id
            );
            Ok(Some(accepted_response(submission_id, true)))
        }
        Ok(Some(IdempotencyOutcome::Reused)) => Err(AppError::IdempotencyReused),
        // `Accepted` is never produced by a lookup; treat it as free so the
        // caller proceeds to the normal queue path.
        Ok(Some(IdempotencyOutcome::Accepted { .. })) | Ok(None) => Ok(None),
        Err(e) => {
            tracing::error!("Failed to look up idempotency: {:?}", e);
            Err(AppError::Internal(
                "Failed to verify idempotency.".to_string(),
            ))
        }
    }
}

/// Route parameters shared by the PATCH/DELETE handlers.
struct CommentWritePath {
    site_id: String,
    page_slug: String,
    comment_id: String,
    fingerprint_path: String,
}

impl CommentWritePath {
    fn new(
        site_id: String,
        page_slug: String,
        comment_id: String,
        fingerprint_path: String,
    ) -> Self {
        Self {
            site_id,
            page_slug,
            comment_id,
            fingerprint_path,
        }
    }
}

pub(crate) async fn query_comments_handler(
    method: Method,
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((site_id, page_slug)): Path<(String, String)>,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    // 1. Only QUERY method is accepted here
    if method != *QUERY_METHOD {
        return Err(AppError::MethodNotAllowed);
    }

    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.public_read_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "public reads are rate limited; try again later".to_string(),
            retry_after_seconds: state.public_read_limiter.window().as_secs(),
        });
    }

    // 2. Validate path params
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug).map_err(AppError::Validation)?;

    // The parent site must exist: a missing parent is a 404, not an empty
    // page, matching the REST convention for nested resources.
    if state
        .store
        .get_site(&site_id_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up site: {e}")))?
        .is_none()
    {
        return Err(AppError::NotFound("Site not found.".to_string()));
    }

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

    tracing::debug!(
        "QUERY comments for site: {}, post: {} (page: {:?}, per_page: {:?})",
        site_id_val.as_str(),
        page_slug_val.as_str(),
        query.page,
        query.per_page,
    );

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = per_page;
    let offset = (page - 1) * per_page;
    let media_base = media_url_base(&state, &headers, Some(addr));

    match state
        .store
        .get_messages(&site_id_val, &page_slug_val, limit, offset)
        .await
    {
        Ok(mut page_data) => {
            if let Some(proxy) = &state.media_proxy {
                for message in &mut page_data.items {
                    proxy.proxify_message(message, &media_base);
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
    Path((site_id, page_slug)): Path<(String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(comment_write_rate_limited(&state));
    }

    let idempotency_key = extract_idempotency_key(&headers)?;
    let mut req: PostCommentRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let fingerprint = request_fingerprint(
        "POST",
        &format!("/api/v1/sites/{}/pages/{}/comments", site_id, page_slug),
        body.as_bytes(),
    );

    // 1. Validate input (struct-level validator enforces non-empty content when media is absent)
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
    if req
        .thread_root
        .as_deref()
        .is_some_and(|thread_root| thread_root.trim().is_empty())
    {
        return Err(AppError::BadRequest(
            "thread_root must not be empty when provided.".to_string(),
        ));
    }
    if let Some(thread_root) = req.thread_root.as_deref()
        && let Err(msg) = validate_thread_root_format(thread_root)
    {
        return Err(AppError::BadRequest(msg.to_string()));
    }
    if let Some(media) = req.media.as_mut() {
        if !media.url.starts_with("mxc://") || media.url.len() > 512 {
            return Err(AppError::BadRequest(
                "media.url must be a valid mxc:// URI.".to_string(),
            ));
        }
        if media.mimetype.as_deref().is_some_and(|m| m.len() > 128)
            || media.filename.as_deref().is_some_and(|f| f.len() > 255)
        {
            return Err(AppError::BadRequest(
                "media metadata is invalid.".to_string(),
            ));
        }
        if media.kind == Some(MediaKind::Sticker) {
            // Stickers must come from the site's projected packs; the server
            // fills the metadata from the pack so visitors cannot forge it.
            let packs =
                state.store.list_site_packs(&site_id).await.map_err(|e| {
                    AppError::Internal(format!("failed to load sticker packs: {e}"))
                })?;
            let Some(image) = packs
                .iter()
                .flat_map(|pack| &pack.pack.content.images)
                .find(|image| image.url == media.url)
            else {
                return Err(AppError::BadRequest(
                    "sticker must reference an image from the site's sticker packs".to_string(),
                ));
            };
            media.filename = Some(
                image
                    .body
                    .clone()
                    .unwrap_or_else(|| image.shortcode.clone()),
            );
            if let Some(info) = &image.info {
                if let Some(mimetype) = info.get("mimetype").and_then(|v| v.as_str()) {
                    media.mimetype = Some(mimetype.to_string());
                }
                if let Some(size) = info.get("size").and_then(|v| v.as_u64()) {
                    media.size = Some(size);
                }
                if let Some(width) = info.get("w").and_then(|v| v.as_u64()) {
                    media.width = u32::try_from(width).ok();
                }
                if let Some(height) = info.get("h").and_then(|v| v.as_u64()) {
                    media.height = u32::try_from(height).ok();
                }
            }
        } else if !state
            .store
            .media_upload_owned_by(&media.url, &req.author_public_key, &site_id, &page_slug)
            .await
            .map_err(|e| AppError::Internal(format!("failed to verify media ownership: {e}")))?
        {
            return Err(AppError::BadRequest(
                "media must reference an upload made by this author for this site and post"
                    .to_string(),
            ));
        }
    }

    // 1b. Idempotency replay short-circuit: an identical request must return
    // the original submission without consuming a new PoW challenge.
    if let Some(response) = idempotency_short_circuit(
        &state,
        &IdempotencyInput {
            author_public_key: req.author_public_key.clone(),
            key: idempotency_key.clone(),
            request_fingerprint: fingerprint.clone(),
        },
    )
    .await?
    {
        return Ok(response);
    }

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 2b. Verify the author's Ed25519 signature over the canonical message.
    let challenge = challenge_prefix(&req.challenge_response);
    let signable_content = req
        .media
        .as_ref()
        .map(|media| media.url.as_str())
        .unwrap_or(req.content.as_str());
    let message = post_signature_message(
        &site_id,
        &page_slug,
        signable_content,
        req.reply_to.as_deref(),
        req.thread_root.as_deref(),
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
                active_target_in_page(&parent, &site_id, &page_slug).map_err(|_| {
                    AppError::BadRequest(
                        "reply_to must reference an active comment in the same site and post."
                            .to_string(),
                    )
                })?;
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
    if let Some(thread_root) = req.thread_root.as_deref() {
        match state.store.get_message(thread_root).await {
            Ok(Some(parent)) => {
                active_target_in_page(&parent, &site_id, &page_slug).map_err(|_| {
                    AppError::BadRequest(
                        "thread_root must reference an active comment in the same site and post."
                            .to_string(),
                    )
                })?;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("Failed to validate thread root: {:?}", e);
                return Err(AppError::Internal(
                    "Failed to validate thread root.".to_string(),
                ));
            }
        }
    }

    // 3. Create the business command
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug).map_err(AppError::Validation)?;

    let command = PostCommentCommand {
        site_id: site_id_val,
        page_slug: page_slug_val,
        content: req.content,
        media: req.media,
        location: None,
        display_name: req.display_name,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
        reply_to: req.reply_to,
        thread_root: req.thread_root,
    };

    // 4. Save the command for the reconciler, atomically with its idempotency
    // record so retries can never queue duplicate work.
    match state
        .store
        .save_post_submission_idempotent(
            &command,
            &IdempotencyInput {
                author_public_key: command.author_public_key.clone(),
                key: idempotency_key,
                request_fingerprint: fingerprint,
            },
        )
        .await
    {
        Ok(IdempotencyOutcome::Accepted { submission_id }) => {
            tracing::debug!("Successfully saved a new a comment submission.");
            state.submission_notify.notify_one();
            Ok(accepted_response(submission_id, false))
        }
        Ok(IdempotencyOutcome::Replayed { submission_id }) => {
            tracing::info!(
                "Replayed idempotent POST with submission_id {}",
                submission_id
            );
            Ok(accepted_response(submission_id, true))
        }
        Ok(IdempotencyOutcome::Reused) => Err(AppError::IdempotencyReused),
        Err(e) => {
            tracing::error!("Failed to save a comment submission: {:?}", e);
            Err(AppError::Internal("Failed to queue comment.".to_string()))
        }
    }
}

/// Delete a comment addressed by its path.
pub(crate) async fn delete_comment_handler(
    State(state): State<ApiState>,
    Path((site_id, page_slug, comment_id)): Path<(String, String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let req: DeleteCommentRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let fingerprint_path =
        format!("/api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}");
    let path = CommentWritePath::new(site_id, page_slug, comment_id, fingerprint_path);
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
        return Err(comment_write_rate_limited(&state));
    }

    let idempotency_key = extract_idempotency_key(&headers)?;
    let fingerprint = request_fingerprint("DELETE", &path.fingerprint_path, body.as_bytes());

    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;
    if let Err(msg) = validate_comment_id_format(&path.comment_id) {
        return Err(AppError::BadRequest(msg.to_string()));
    }

    // 1b. Idempotency replay short-circuit.
    if let Some(response) = idempotency_short_circuit(
        &state,
        &IdempotencyInput {
            author_public_key: req.author_public_key.clone(),
            key: idempotency_key.clone(),
            request_fingerprint: fingerprint.clone(),
        },
    )
    .await?
    {
        return Ok(response);
    }

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 2b. Verify the author's Ed25519 signature.
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        Some("DELETE"),
        Some(path.site_id.as_str()),
        Some(path.page_slug.as_str()),
        Some(path.comment_id.as_str()),
        Some(challenge),
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    // 2c. Authorization: the presented public key must be the comment's owner.
    match state.store.get_message(&path.comment_id).await {
        Ok(Some(message)) => {
            if message.site_id != path.site_id || message.page_slug != path.page_slug {
                return Err(AppError::NotFound("Comment not found.".to_string()));
            }
            if message.author.kind == AuthorKind::Matrix {
                return Err(AppError::NotManageable(
                    "This comment was posted by a Matrix user; manage it from a Matrix client."
                        .to_string(),
                ));
            }
            if message.status != MessageStatus::Active {
                return Err(AppError::Conflict(
                    "The target comment has been deleted.".to_string(),
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

    // 3. Create the business command
    let site_id_val = SiteId::new(path.site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(path.page_slug).map_err(AppError::Validation)?;
    let command = DeleteCommentCommand {
        site_id: site_id_val,
        page_slug: page_slug_val,
        event_id: path.comment_id,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
    };

    // 4. Save the command for the reconciler, atomically with its idempotency
    // record so retries can never queue duplicate work.
    match state
        .store
        .save_delete_submission_idempotent(
            &command,
            &IdempotencyInput {
                author_public_key: command.author_public_key.clone(),
                key: idempotency_key,
                request_fingerprint: fingerprint,
            },
        )
        .await
    {
        Ok(IdempotencyOutcome::Accepted { submission_id }) => {
            tracing::debug!("Successfully saved a delete a comment submission.");
            state.submission_notify.notify_one();
            Ok(accepted_response(submission_id, false))
        }
        Ok(IdempotencyOutcome::Replayed { submission_id }) => {
            tracing::info!(
                "Replayed idempotent DELETE with submission_id {}",
                submission_id
            );
            Ok(accepted_response(submission_id, true))
        }
        Ok(IdempotencyOutcome::Reused) => Err(AppError::IdempotencyReused),
        Err(e) => {
            tracing::error!("Failed to save delete a comment submission: {:?}", e);
            Err(AppError::Internal(
                "Failed to queue delete request.".to_string(),
            ))
        }
    }
}

/// The handler for receiving a new update comment request.
pub(crate) async fn update_comment_handler(
    State(state): State<ApiState>,
    Path((site_id, page_slug, comment_id)): Path<(String, String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let req: UpdateCommentRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let fingerprint_path =
        format!("/api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}");
    let path = CommentWritePath::new(site_id, page_slug, comment_id, fingerprint_path);
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
        return Err(comment_write_rate_limited(&state));
    }

    let idempotency_key = extract_idempotency_key(&headers)?;
    let fingerprint = request_fingerprint("PATCH", &path.fingerprint_path, body.as_bytes());

    // 1. Validate input
    req.validate().map_err(AppError::Validation)?;
    if let Err(msg) = validate_comment_id_format(&path.comment_id) {
        return Err(AppError::BadRequest(msg.to_string()));
    }

    // 1b. Idempotency replay short-circuit.
    if let Some(response) = idempotency_short_circuit(
        &state,
        &IdempotencyInput {
            author_public_key: req.author_public_key.clone(),
            key: idempotency_key.clone(),
            request_fingerprint: fingerprint.clone(),
        },
    )
    .await?
    {
        return Ok(response);
    }

    // 2. Verify the PoW challenge
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }

    // 3b. Verify the author's Ed25519 signature.
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        Some("PATCH"),
        Some(path.site_id.as_str()),
        Some(path.page_slug.as_str()),
        Some(path.comment_id.as_str()),
        Some(req.content.as_str()),
        Some(challenge),
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    // 3c. Authorization: the presented public key must be the comment's owner,
    // and the comment must belong to the site/post in the path.
    match state.store.get_message(&path.comment_id).await {
        Ok(Some(message)) => {
            if message.site_id != path.site_id || message.page_slug != path.page_slug {
                return Err(AppError::NotFound("Comment not found.".to_string()));
            }
            if message.author.kind == AuthorKind::Matrix {
                return Err(AppError::NotManageable(
                    "This comment was posted by a Matrix user; manage it from a Matrix client."
                        .to_string(),
                ));
            }
            if message.status != MessageStatus::Active {
                return Err(AppError::Conflict(
                    "The target comment has been deleted.".to_string(),
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

    // 4. Create the business command
    let site_id_val = SiteId::new(path.site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(path.page_slug).map_err(AppError::Validation)?;
    let command = UpdateCommentCommand {
        site_id: site_id_val,
        page_slug: page_slug_val,
        event_id: path.comment_id,
        content: req.content,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
    };

    // 5. Save the command for the reconciler, atomically with its idempotency
    // record so retries can never queue duplicate work.
    match state
        .store
        .save_update_submission_idempotent(
            &command,
            &IdempotencyInput {
                author_public_key: command.author_public_key.clone(),
                key: idempotency_key,
                request_fingerprint: fingerprint,
            },
        )
        .await
    {
        Ok(IdempotencyOutcome::Accepted { submission_id }) => {
            tracing::debug!("Successfully saved an update a comment submission.");
            state.submission_notify.notify_one();
            Ok(accepted_response(submission_id, false))
        }
        Ok(IdempotencyOutcome::Replayed { submission_id }) => {
            tracing::info!(
                "Replayed idempotent PATCH with submission_id {}",
                submission_id
            );
            Ok(accepted_response(submission_id, true))
        }
        Ok(IdempotencyOutcome::Reused) => Err(AppError::IdempotencyReused),
        Err(e) => {
            tracing::error!("Failed to save update a comment submission: {:?}", e);
            Err(AppError::Internal(
                "Failed to queue update request.".to_string(),
            ))
        }
    }
}

/// `POST /api/v1/sites/{site}/pages/{post}/comments/{comment_id}/reactions`
pub(crate) async fn react_handler(
    State(state): State<ApiState>,
    Path((site_id, page_slug, comment_id)): Path<(String, String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(comment_write_rate_limited(&state));
    }
    let req: ReactRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    req.validate().map_err(AppError::Validation)?;
    let normalized_key = normalize_reaction_key(&req.key)?;
    if let Err(msg) = validate_comment_id_format(&comment_id) {
        return Err(AppError::BadRequest(msg.to_string()));
    }
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        Some("REACT"),
        Some(site_id.as_str()),
        Some(page_slug.as_str()),
        Some(comment_id.as_str()),
        Some(normalized_key.as_str()),
        Some(challenge),
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug).map_err(AppError::Validation)?;
    let target = state
        .store
        .get_message(&comment_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to verify target: {e}")))?;
    active_target_in_page(
        target
            .as_ref()
            .ok_or_else(|| AppError::NotFound("Comment not found.".to_string()))?,
        site_id_val.as_str(),
        page_slug_val.as_str(),
    )?;
    let Some(room_id) = state
        .store
        .get_registered_room(&site_id_val, &page_slug_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve room: {e}")))?
    else {
        return Err(AppError::NotFound(
            "No room registered for this post.".to_string(),
        ));
    };
    let result = state
        .driver
        .react_message(
            &room_id,
            &comment_id,
            &normalized_key,
            &site_id_val,
            &req.author_public_key,
            &req.author_signature,
            challenge,
            &deterministic_transaction_id(
                "react",
                &[
                    site_id_val.as_str(),
                    page_slug_val.as_str(),
                    room_id.as_str(),
                    comment_id.as_str(),
                    req.author_public_key.as_str(),
                    normalized_key.as_str(),
                    req.challenge_response.as_str(),
                ],
            ),
        )
        .await;
    match result {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("M_DUPLICATE_ANNOTATION") {
                // Idempotent: already reacted with this key.
                return Ok(StatusCode::NO_CONTENT);
            }
            Err(AppError::Internal(format!(
                "failed to send reaction: {msg}"
            )))
        }
    }
}

/// `DELETE /api/v1/sites/{site}/pages/{post}/comments/{comment_id}/reactions/{key}`
pub(crate) async fn unreact_handler(
    State(state): State<ApiState>,
    Path((site_id, page_slug, comment_id, key)): Path<(String, String, String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let client = client_key(&headers, Some(connect.0), &state.trusted_proxies);
    if !state.write_limiter.allow(&client) {
        return Err(comment_write_rate_limited(&state));
    }
    let req: UnreactRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    req.validate().map_err(AppError::Validation)?;
    let normalized_key = normalize_reaction_key(&key)?;
    if let Err(msg) = validate_comment_id_format(&comment_id) {
        return Err(AppError::BadRequest(msg.to_string()));
    }
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        Some("UNREACT"),
        Some(site_id.as_str()),
        Some(page_slug.as_str()),
        Some(comment_id.as_str()),
        Some(normalized_key.as_str()),
        Some(challenge),
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug).map_err(AppError::Validation)?;
    let target = state
        .store
        .get_message(&comment_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to verify target: {e}")))?;
    let target_msg = target
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Comment not found.".to_string()))?;
    // Allow unreact even if the target is redacted? No, treat as NotFound for consistency.
    if target_msg.status == MessageStatus::Redacted {
        return Err(AppError::NotFound("Comment not found.".to_string()));
    }
    active_target_in_page(target_msg, site_id_val.as_str(), page_slug_val.as_str())?;
    let Some(room_id) = state
        .store
        .get_registered_room(&site_id_val, &page_slug_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve room: {e}")))?
    else {
        return Err(AppError::NotFound(
            "No room registered for this post.".to_string(),
        ));
    };
    // Resolve the virtual user MXID for this author to find the reaction row.
    let server_name = state.server_name.clone().or_else(|| {
        state
            .driver
            .sender_user_id()
            .and_then(|mxid| mxid.split(':').nth(1).map(|s| s.to_string()))
    });
    let Some(server_name) = server_name else {
        return Err(AppError::Internal(
            "server_name not configured; cannot resolve virtual user".to_string(),
        ));
    };
    let visitor_id = derive_visitor_id_from_public_key(&req.author_public_key)
        .ok_or_else(|| AppError::BadRequest("invalid author_public_key".to_string()))?;
    let virtual_user = format!(
        "@_cumments_{}_{}:{}",
        site_id_val.as_str(),
        visitor_id,
        server_name
    );
    let reaction = state
        .store
        .find_reaction_by_sender_and_key(&comment_id, &virtual_user, &normalized_key)
        .await
        .map_err(|e| AppError::Internal(format!("failed to lookup reaction: {e}")))?;
    let Some(reaction) = reaction else {
        // Idempotent delete: already removed. Return 204 to avoid leaking existence.
        return Ok(StatusCode::NO_CONTENT);
    };
    // Redact the reaction event via the AS sender (has redact power).
    let proof = serde_json::json!({
        "host.curious.cumments": {
            "site_id": site_id_val.as_str(),
            "page_slug": page_slug_val.as_str(),
            "target_event_id": reaction.event_id,
            "key": normalized_key,
            "public_key": req.author_public_key,
            "signature": req.author_signature,
            "challenge": challenge,
        }
    });
    let txn_id = deterministic_transaction_id(
        "unreact",
        &[
            site_id_val.as_str(),
            page_slug_val.as_str(),
            room_id.as_str(),
            comment_id.as_str(),
            req.author_public_key.as_str(),
            normalized_key.as_str(),
            req.challenge_response.as_str(),
        ],
    );
    let result = state
        .driver
        .redact_message(&room_id, &reaction.event_id, None, Some(&proof), &txn_id)
        .await;
    match result {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("M_NOT_FOUND") || msg.contains("M_UNKNOWN") {
                // Already redacted on homeserver.
                return Ok(StatusCode::NO_CONTENT);
            }
            Err(AppError::Internal(format!(
                "failed to remove reaction: {msg}"
            )))
        }
    }
}

/// `POST /api/v1/sites/{site}/pages/{post}/polls/{poll_id}/votes`
pub(crate) async fn vote_handler(
    State(state): State<ApiState>,
    Path((site_id, page_slug, poll_id)): Path<(String, String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(comment_write_rate_limited(&state));
    }
    let req: VoteRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    req.validate().map_err(AppError::Validation)?;
    if let Err(msg) = validate_comment_id_format(&poll_id) {
        return Err(AppError::BadRequest(msg.to_string()));
    }
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }
    let challenge = challenge_prefix(&req.challenge_response);
    let message = signature_message(&[
        Some("VOTE"),
        Some(site_id.as_str()),
        Some(page_slug.as_str()),
        Some(poll_id.as_str()),
        Some(req.option_id.as_str()),
        Some(challenge),
    ]);
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }

    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug).map_err(AppError::Validation)?;
    let Some(poll_message) = state
        .store
        .get_message(&poll_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to verify poll: {e}")))?
    else {
        return Err(AppError::NotFound("Poll not found.".to_string()));
    };
    active_target_in_page(&poll_message, site_id_val.as_str(), page_slug_val.as_str())?;
    let Content::Poll(poll) = &poll_message.content else {
        return Err(AppError::BadRequest("target is not a poll".to_string()));
    };
    if !poll.options.iter().any(|option| option.id == req.option_id) {
        return Err(AppError::BadRequest("poll option not found".to_string()));
    }
    let Some(room_id) = state
        .store
        .get_registered_room(&site_id_val, &page_slug_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve room: {e}")))?
    else {
        return Err(AppError::NotFound(
            "No room registered for this post.".to_string(),
        ));
    };
    state
        .driver
        .vote_poll(
            &room_id,
            &poll_id,
            &req.option_id,
            &site_id_val,
            &req.author_public_key,
            &req.author_signature,
            challenge,
            &deterministic_transaction_id(
                "vote",
                &[
                    site_id_val.as_str(),
                    page_slug_val.as_str(),
                    room_id.as_str(),
                    poll_id.as_str(),
                    req.author_public_key.as_str(),
                    req.option_id.as_str(),
                    req.challenge_response.as_str(),
                ],
            ),
        )
        .await
        .map_err(|e| AppError::Internal(format!("failed to send vote: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/sites/{site}/pages/{post}/location`
pub(crate) async fn location_handler(
    State(state): State<ApiState>,
    Path((site_id, page_slug)): Path<(String, String)>,
    connect: ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(comment_write_rate_limited(&state));
    }
    let idempotency_key = extract_idempotency_key(&headers)?;
    let req: LocationRequest = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {}", e)))?;
    let fingerprint = request_fingerprint(
        "POST",
        &format!("/api/v1/sites/{}/pages/{}/location", site_id, page_slug),
        body.as_bytes(),
    );
    req.validate().map_err(AppError::Validation)?;
    if !is_valid_geo_uri(&req.geo_uri) {
        return Err(AppError::BadRequest(
            "geo_uri must be a geo: URI with decimal lat,lon coordinates".to_string(),
        ));
    }
    // 1b. Idempotency replay short-circuit.
    if let Some(response) = idempotency_short_circuit(
        &state,
        &IdempotencyInput {
            author_public_key: req.author_public_key.clone(),
            key: idempotency_key.clone(),
            request_fingerprint: fingerprint.clone(),
        },
    )
    .await?
    {
        return Ok(response);
    }
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
    if req
        .thread_root
        .as_deref()
        .is_some_and(|thread_root| thread_root.trim().is_empty())
    {
        return Err(AppError::BadRequest(
            "thread_root must not be empty when provided.".to_string(),
        ));
    }
    if let Some(thread_root) = req.thread_root.as_deref()
        && let Err(msg) = validate_thread_root_format(thread_root)
    {
        return Err(AppError::BadRequest(msg.to_string()));
    }
    if !state.pow.verify(&req.challenge_response) {
        return Err(AppError::InvalidPoW);
    }
    let challenge = challenge_prefix(&req.challenge_response);
    let message = cumments_core::identity::locate_signature_message(
        &site_id,
        &page_slug,
        &req.geo_uri,
        req.reply_to.as_deref(),
        req.thread_root.as_deref(),
        challenge,
    );
    if !verify_signature(&req.author_public_key, &message, &req.author_signature) {
        return Err(AppError::InvalidSignature);
    }
    if let Some(reply_to) = req.reply_to.as_deref() {
        match state.store.get_message(reply_to).await {
            Ok(Some(parent)) => {
                active_target_in_page(&parent, &site_id, &page_slug).map_err(|_| {
                    AppError::BadRequest(
                        "reply_to must reference an active comment in the same site and post."
                            .to_string(),
                    )
                })?;
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
    if let Some(thread_root) = req.thread_root.as_deref() {
        match state.store.get_message(thread_root).await {
            Ok(Some(parent)) => {
                active_target_in_page(&parent, &site_id, &page_slug).map_err(|_| {
                    AppError::BadRequest(
                        "thread_root must reference an active comment in the same site and post."
                            .to_string(),
                    )
                })?;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("Failed to validate thread root: {:?}", e);
                return Err(AppError::Internal(
                    "Failed to validate thread root.".to_string(),
                ));
            }
        }
    }

    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug).map_err(AppError::Validation)?;
    let command = PostCommentCommand {
        site_id: site_id_val,
        page_slug: page_slug_val,
        content: String::new(),
        media: None,
        location: Some(LocationPayload {
            geo_uri: req.geo_uri,
            description: req.description,
        }),
        display_name: req.display_name,
        author_public_key: req.author_public_key,
        author_signature: req.author_signature,
        author_challenge: challenge.to_string(),
        reply_to: req.reply_to,
        thread_root: req.thread_root,
    };

    match state
        .store
        .save_post_submission_idempotent(
            &command,
            &IdempotencyInput {
                author_public_key: command.author_public_key.clone(),
                key: idempotency_key,
                request_fingerprint: fingerprint,
            },
        )
        .await
    {
        Ok(IdempotencyOutcome::Accepted { submission_id }) => {
            tracing::debug!("Successfully saved a new location submission.");
            state.submission_notify.notify_one();
            Ok(accepted_response(submission_id, false))
        }
        Ok(IdempotencyOutcome::Replayed { submission_id }) => {
            tracing::info!(
                "Replayed idempotent LOCATE with submission_id {}",
                submission_id
            );
            Ok(accepted_response(submission_id, true))
        }
        Ok(IdempotencyOutcome::Reused) => Err(AppError::IdempotencyReused),
        Err(e) => {
            tracing::error!("Failed to save location submission: {:?}", e);
            Err(AppError::Internal("Failed to queue location.".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::IDEMPOTENCY_KEY_HEADER;

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
        let first = request_fingerprint("POST", "/api/v1/pages/p", b"{}");
        assert_eq!(first, request_fingerprint("POST", "/api/v1/pages/p", b"{}"));
        assert_ne!(
            first,
            request_fingerprint("POST", "/api/v1/pages/p", b"{ }")
        );
        assert_ne!(
            first,
            request_fingerprint("PATCH", "/api/v1/pages/p", b"{}")
        );
        assert_ne!(
            first,
            request_fingerprint("POST", "/api/v1/pages/other", b"{}")
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

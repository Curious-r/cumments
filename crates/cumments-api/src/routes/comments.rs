//! Comment query/post/delete/update route handlers.

use crate::ApiState;
use crate::error::AppError;
use crate::request::{
    DeleteCommentRequest, PaginatedResponse, PaginationMeta, PaginationQuery, PostCommentRequest,
    UpdateCommentRequest,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderName, Method, StatusCode},
    response::IntoResponse,
};
use cumments_core::{
    identity::{post_signature_message, signature_message, verify_signature},
    intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent},
    models::{AuthorType, PostSlug, SiteId},
};
use ruma_common::EventId;
use validator::Validate;

pub(crate) static QUERY_METHOD: std::sync::LazyLock<Method> =
    std::sync::LazyLock::new(|| Method::from_bytes(b"QUERY").unwrap());

/// The Accept-Query response header (RFC 10008, Section 3).
static ACCEPT_QUERY: std::sync::LazyLock<HeaderName> =
    std::sync::LazyLock::new(|| HeaderName::from_static("accept-query"));

/// Build the CORS layer from a comma-separated origin list.
fn challenge_prefix(challenge_response: &str) -> &str {
    challenge_response.split('|').next().unwrap_or("")
}

/// Error returned when `reply_to` cannot be a Matrix event ID.
const REPLY_TO_FORMAT_ERROR: &str =
    "reply_to must be a Matrix event ID (e.g. \"$event\" or \"$event:server\")";

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
        .get_comments(&site_id_val, &post_slug_val, limit, offset)
        .await
    {
        Ok(page_data) => {
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
    Json(req): Json<PostCommentRequest>,
) -> Result<impl IntoResponse, AppError> {
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
        match state.store.get_comment(reply_to).await {
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
pub(crate) async fn delete_comment_handler(
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
            if comment.author.kind == AuthorType::Matrix {
                return Err(AppError::NotManageable(
                    "This comment was posted by a Matrix user; manage it from a Matrix client."
                        .to_string(),
                ));
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
        author_challenge: challenge.to_string(),
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
pub(crate) async fn update_comment_handler(
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
            if comment.author.kind == AuthorType::Matrix {
                return Err(AppError::NotManageable(
                    "This comment was posted by a Matrix user; manage it from a Matrix client."
                        .to_string(),
                ));
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
}

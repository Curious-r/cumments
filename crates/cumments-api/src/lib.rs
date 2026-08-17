use crate::routes::comments::{
    delete_comment_handler, location_handler, post_comment_handler, query_comments_handler,
    react_handler, update_comment_body_handler, update_comment_handler, vote_handler,
};
use crate::routes::governance::{
    add_global_moderator_handler, add_owner_handler, add_room_moderator_handler,
    list_room_moderators_handler, list_site_roles_handler, remove_global_moderator_handler,
    remove_owner_handler, remove_room_moderator_handler, require_claim_token,
    retire_page_room_handler, retire_site_handler, upgrade_page_room_handler,
};
use crate::routes::media::{
    MEDIA_MAX_BYTES, MediaProxy, add_site_sticker_handler, delete_visitor_avatar_handler,
    list_stickers_handler, media_handler, remove_site_sticker_handler, set_visitor_avatar_handler,
    upload_media_handler,
};
use crate::routes::misc::{get_challenge_handler, health_handler};
use crate::routes::operator::{
    config_snippet_handler, list_operator_sites_handler, list_quarantined_rooms_handler,
    reinstate_room_handler, require_operator, retire_room_handler, revoke_secret_handler,
    revoke_verified_origin_handler, rotate_claim_token_handler, rotate_secret_handler,
    upgrade_room_handler,
};
use crate::routes::room::room_info_handler;
use crate::routes::sites::{
    confirm_verification_handler, issue_secret_handler, register_site_handler,
    start_verification_handler,
};
use crate::routes::sse::SseReconnectRegistry;
use crate::routes::sse::sse_handler;
use crate::routes::visitors::visitor_profile_handler;
use crate::site_auth::{enforce_site_auth, public_cors};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, patch, post, put},
};
use cumments_core::{
    ephemeral::{EphemeralEvent, EphemeralState},
    ports::{
        GovernanceStore, MatrixDriver, MessageStore, RegistryStore, RoleClaimStore, RoomStore,
        SiteAuthStore, SiteStore, StickerPackStore, SubmissionStore, VirtualUserStore,
    },
    projector_events::ProjectorEvent,
    site_auth::SiteAuthPolicy,
    site_service::SiteService,
};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, broadcast};
use tower_http::trace::TraceLayer;

pub mod error;
pub mod pow;
pub mod rate_limit;
pub mod request;
pub mod routes;
pub mod site_auth;
pub mod trusted_proxy;

// ----------------------

// Define a new trait that combines the store traits for API use.
pub trait ApiStore:
    MessageStore
    + SubmissionStore
    + SiteStore
    + SiteAuthStore
    + RegistryStore
    + RoomStore
    + GovernanceStore
    + StickerPackStore
    + RoleClaimStore
    + VirtualUserStore
    + Send
    + Sync
{
}
impl<
    T: MessageStore
        + SubmissionStore
        + SiteStore
        + SiteAuthStore
        + RegistryStore
        + RoomStore
        + GovernanceStore
        + StickerPackStore
        + RoleClaimStore
        + VirtualUserStore
        + Send
        + Sync,
> ApiStore for T
{
}

// The shared state for our API.
#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<dyn ApiStore>,
    pub driver: Arc<dyn MatrixDriver>,
    pub site_service: Arc<SiteService>,
    pub pow: Arc<pow::Pow>,
    pub event_bus: broadcast::Sender<ProjectorEvent>,
    /// Wakes the submission reconcile passes when a write submission is saved.
    pub submission_notify: Arc<Notify>,
    /// Wakes the site-governance passes when the API writes governance state
    /// (role writes, site retirement).
    pub governance_notify: Arc<Notify>,
    /// Instance-wide site verification policy plus the operator-declared
    /// per-site overlay.
    pub site_auth_policy: Arc<SiteAuthPolicy>,
    /// SHA-256 hash of the operator operator token, when enabled.
    pub operator_token_hash: Option<String>,
    /// Anti-spam limiter for open site registration.
    pub registration_limiter: Arc<rate_limit::RateLimiter>,
    /// Anti-spam limiter for verification token issuance.
    pub verification_limiter: Arc<rate_limit::RateLimiter>,
    /// Anti-brute-force limiter for the Operator API.
    pub operator_limiter: Arc<rate_limit::RateLimiter>,
    /// Anti-abuse limiter for verification confirm (outbound probes).
    pub confirm_limiter: Arc<rate_limit::RateLimiter>,
    /// Reverse proxies trusted to set `X-Forwarded-For` for rate limiting.
    pub trusted_proxies: Arc<trusted_proxy::TrustedProxySet>,
    /// Allow verification of loopback/private/link-local IP-literal origins.
    pub allow_private_verification_origins: bool,
    /// Per-client-key limiter for comment and visitor-avatar write submissions
    /// (POST/PUT/PATCH/DELETE).
    pub write_limiter: Arc<rate_limit::RateLimiter>,
    /// Per-client-key limiter for new SSE connections.
    pub sse_limiter: Arc<rate_limit::RateLimiter>,
    /// Reconnect bookkeeping for SSE, so EventSource reconnects do not count
    /// against the new-connection budget.
    pub sse_reconnect: Arc<Mutex<SseReconnectRegistry>>,
    /// Global cap on concurrent SSE connections.
    pub max_sse_connections: usize,
    /// Live SSE connection count.
    pub active_sse_connections: Arc<AtomicUsize>,
    /// Optional public media proxy for Matrix MXC media.
    pub media_proxy: Option<Arc<MediaProxy>>,
    /// Per-client-key limiter for media proxy requests.
    pub media_limiter: Arc<rate_limit::RateLimiter>,
    /// Per-client-key limiter for the public visitor profile endpoint.
    pub visitor_profile_limiter: Arc<rate_limit::RateLimiter>,
    /// Per-client-key limiter for local public read endpoints (comment
    /// lists, room metadata, roles, moderators, sticker packs).
    pub public_read_limiter: Arc<rate_limit::RateLimiter>,
    /// Per-client-key limiter for site governance writes.
    pub governance_limiter: Arc<rate_limit::RateLimiter>,
    /// Live ephemeral events (typing/receipts/presence) for SSE.
    pub ephemeral_bus: broadcast::Sender<EphemeralEvent>,
    /// Shared typing state for SSE snapshots, when ephemeral sync is enabled.
    pub ephemeral_state: Option<Arc<EphemeralState>>,
}

/// Builds the Axum router for the API.
pub fn build_router(state: ApiState) -> Router {
    // Comment routes: writes are gated by site auth; QUERY reads stay public
    // and get `Access-Control-Allow-Origin: *`.
    let comment_router = Router::new()
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/comments",
            // POST for writing submissions, fallback handles QUERY for reading.
            post(post_comment_handler)
                .patch(update_comment_body_handler)
                .delete(delete_comment_handler)
                .fallback(query_comments_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}",
            patch(update_comment_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}/reactions",
            post(react_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/polls/{poll_id}/votes",
            post(vote_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/media",
            // The upload handler enforces the 20MB cap itself; raise axum's
            // default 2MB extractor limit accordingly so large uploads reach
            // it instead of failing at extraction.
            post(upload_media_handler)
                .fallback(method_not_allowed_handler)
                .layer(DefaultBodyLimit::max(MEDIA_MAX_BYTES)),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/location",
            post(location_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/visitors/avatar",
            put(set_visitor_avatar_handler)
                .delete(delete_visitor_avatar_handler)
                .fallback(method_not_allowed_handler)
                .layer(DefaultBodyLimit::max(MEDIA_MAX_BYTES)),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            enforce_site_auth,
        ));

    // Public routes: comments are public data, registration and verification
    // are self-service, and `/health` is an infrastructure endpoint.
    let public_router = Router::new()
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/sse",
            get(sse_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/challenge",
            get(get_challenge_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/room",
            get(room_info_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/stickers",
            get(list_stickers_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/visitors/profile",
            get(visitor_profile_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/roles",
            get(list_site_roles_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/moderators",
            get(list_room_moderators_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/media/{server}/{media_id}",
            get(media_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites",
            post(register_site_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/verifications",
            post(start_verification_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/verifications/confirm",
            post(confirm_verification_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/secret",
            post(issue_secret_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/health",
            get(health_handler).fallback(method_not_allowed_handler),
        )
        .layer(middleware::from_fn(public_cors));

    // Site governance writes, authenticated with the site's claim token.
    let governance_router = Router::new()
        .route(
            "/api/v1/sites/{site_id}/owners",
            post(add_owner_handler).delete(remove_owner_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/global-moderators",
            post(add_global_moderator_handler).delete(remove_global_moderator_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/moderators",
            post(add_room_moderator_handler).delete(remove_room_moderator_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/packs/{pack_id}/stickers",
            post(add_site_sticker_handler).delete(remove_site_sticker_handler),
        )
        .route(
            "/api/v1/sites/{site_id}",
            axum::routing::delete(retire_site_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/upgrade",
            axum::routing::post(upgrade_page_room_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}",
            axum::routing::delete(retire_page_room_handler).fallback(method_not_allowed_handler),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_claim_token,
        ));

    let operator_router = Router::new()
        .route(
            "/api/v1/operator/sites",
            axum::routing::get(method_not_allowed_handler).fallback(list_operator_sites_handler),
        )
        .route(
            "/api/v1/operator/sites/{site_id}/origins/revoke",
            axum::routing::post(revoke_verified_origin_handler)
                .fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/sites/{site_id}/secret/rotate",
            axum::routing::post(rotate_secret_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/sites/{site_id}/secret",
            axum::routing::delete(revoke_secret_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/sites/{site_id}/config-snippet",
            axum::routing::get(config_snippet_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/sites/{site_id}/claim-token/rotate",
            axum::routing::post(rotate_claim_token_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/sites/{site_id}",
            axum::routing::delete(retire_site_handler).fallback(method_not_allowed_handler),
        )
        // Operator fallback for site ownership takeover.
        .route(
            "/api/v1/operator/sites/{site_id}/owners",
            axum::routing::post(add_owner_handler)
                .delete(remove_owner_handler)
                .fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/sites/{site_id}/global-moderators",
            axum::routing::post(add_global_moderator_handler)
                .delete(remove_global_moderator_handler)
                .fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/sites/{site_id}/packs/{pack_id}/stickers",
            axum::routing::post(add_site_sticker_handler)
                .delete(remove_site_sticker_handler)
                .fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/rooms/quarantined",
            axum::routing::get(method_not_allowed_handler).fallback(list_quarantined_rooms_handler),
        )
        .route(
            "/api/v1/operator/rooms/quarantined/{room_id}",
            axum::routing::delete(reinstate_room_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/rooms/{room_id}/upgrade",
            axum::routing::post(upgrade_room_handler).fallback(method_not_allowed_handler),
        )
        .route(
            "/api/v1/operator/rooms/{room_id}",
            axum::routing::delete(retire_room_handler).fallback(method_not_allowed_handler),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_operator,
        ));

    Router::new()
        .merge(comment_router)
        .merge(governance_router)
        .merge(public_router)
        .merge(operator_router)
        .fallback(not_found_handler)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Unmatched methods on a registered route return the same problem envelope
/// as business errors.
async fn method_not_allowed_handler() -> error::AppError {
    error::AppError::MethodNotAllowed
}

/// Unmatched paths return the same problem envelope as business errors.
async fn not_found_handler() -> error::AppError {
    error::AppError::NotFound("Route not found.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use crate::request::PaginationQuery;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use cumments_core::models::{
        AuthorKind, AuthorSnapshot, Content, Message, MessageStatus, TextContent, TextStyle,
    };
    use validator::Validate;

    #[test]
    fn api_state_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ApiState>();
    }

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
    fn not_manageable_error_is_forbidden_with_dedicated_code() {
        let response = AppError::NotManageable("matrix-owned".to_string()).into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn message_serializes_nested_author_and_hides_internal_fields() {
        let message = Message {
            event_id: "$e:hs".to_string(),
            site_id: "my-blog".to_string(),
            page_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Visitor,
                display_name: Some("Alice".to_string()),
                avatar_url: None,
                public_key: Some("pk".to_string()),
                mxid: None,
            },
            content: Content::Text(TextContent {
                body: "hi".to_string(),
                formatted_body: None,
                style: TextStyle::Normal,
            }),
            timestamp: chrono::Utc::now(),
            edited_at: None,
            reply_to: None,
            thread_root: None,
            submission_id: Some(42),
            status: MessageStatus::Active,
            redacted_at: None,
            redacted_by: None,
            reactions: Vec::new(),
            room_id: "!room:hs".to_string(),
            sender_mxid: "@_cumments_my-blog_abcd:hs".to_string(),
            raw_content: serde_json::Value::Null,
        };

        let json = serde_json::to_value(&message).expect("serialize message");
        assert_eq!(json["author"]["type"], "visitor");
        assert_eq!(json["author"]["public_key"], "pk");
        assert_eq!(json["content"]["type"], "text");
        assert_eq!(json["content"]["body"], "hi");
        assert_eq!(json["status"], "active");
        assert_eq!(json["submission_id"], 42);
        assert!(json.get("sender_mxid").is_none());
        assert!(json.get("room_id").is_none());
    }
}

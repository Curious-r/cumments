use crate::routes::admin::{
    config_snippet_handler, list_admin_sites_handler, require_admin, revoke_secret_handler,
    revoke_verified_origin_handler, rotate_secret_handler,
};
use crate::routes::comments::{
    delete_comment_handler, post_comment_handler, query_comments_handler, update_comment_handler,
};
use crate::routes::misc::{get_challenge_handler, health_handler};
use crate::routes::sites::{
    confirm_verification_handler, issue_secret_handler, register_site_handler,
    start_verification_handler,
};
use crate::routes::sse::sse_handler;
use crate::site_auth::{enforce_site_auth, public_cors};
use axum::{
    Router, middleware,
    routing::{delete, get, post},
};
use cumments_core::{
    ports::{CommentStore, IntentStore, SiteAuthStore, SiteStore},
    projector_events::ProjectorEvent,
    site_auth::SiteAuthPolicy,
};
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::{Notify, broadcast};
use tower_http::trace::TraceLayer;

pub mod error;
pub mod pow;
pub mod rate_limit;
pub mod request;
pub mod routes;
pub mod site_auth;

// ----------------------

// Define a new trait that combines the store traits for API use.
pub trait ApiStore: CommentStore + IntentStore + SiteStore + SiteAuthStore + Send + Sync {}
impl<T: CommentStore + IntentStore + SiteStore + SiteAuthStore + Send + Sync> ApiStore for T {}

// The shared state for our API.
#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<dyn ApiStore>,
    pub pow: Arc<pow::Pow>,
    pub event_bus: broadcast::Sender<ProjectorEvent>,
    pub reconciler_notify: Arc<Notify>,
    /// Instance-wide site verification policy plus the operator-declared
    /// per-site overlay.
    pub site_auth_policy: Arc<SiteAuthPolicy>,
    /// SHA-256 hash of the operator admin token, when enabled.
    pub admin_token_hash: Option<String>,
    /// Anti-spam limiter for open site registration.
    pub registration_limiter: Arc<rate_limit::RateLimiter>,
    /// Anti-spam limiter for verification token issuance.
    pub verification_limiter: Arc<rate_limit::RateLimiter>,
    /// Anti-brute-force limiter for the admin API.
    pub admin_limiter: Arc<rate_limit::RateLimiter>,
    /// Reverse proxies trusted to set `X-Forwarded-For` for rate limiting.
    pub trusted_proxies: Arc<HashSet<IpAddr>>,
}

/// Builds the Axum router for the API.
pub fn build_router(state: ApiState) -> Router {
    // Comment routes: writes are gated by site auth; QUERY reads stay public
    // and get `Access-Control-Allow-Origin: *`.
    let comment_router = Router::new()
        .route(
            "/api/v1/sites/{site_id}/posts/{post_slug}/comments",
            // POST for writing intents, fallback handles QUERY for reading.
            post(post_comment_handler).fallback(query_comments_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/posts/{post_slug}/comments/{comment_id}",
            delete(delete_comment_handler).patch(update_comment_handler),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            enforce_site_auth,
        ));

    // Public routes: comments are public data, registration and verification
    // are self-service, and `/health` is an infrastructure endpoint.
    let public_router = Router::new()
        .route(
            "/api/v1/sites/{site_id}/posts/{post_slug}/sse",
            get(sse_handler),
        )
        .route("/api/v1/challenge", get(get_challenge_handler))
        .route("/api/v1/sites", post(register_site_handler))
        .route(
            "/api/v1/sites/{site_id}/verifications",
            post(start_verification_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/verifications/confirm",
            post(confirm_verification_handler),
        )
        .route("/api/v1/sites/{site_id}/secret", post(issue_secret_handler))
        .route("/health", get(health_handler))
        .layer(middleware::from_fn(public_cors));

    let admin_router = Router::new()
        .route(
            "/api/v1/admin/sites",
            axum::routing::get(list_admin_sites_handler),
        )
        .route(
            "/api/v1/admin/sites/{site_id}/origins/revoke",
            axum::routing::post(revoke_verified_origin_handler),
        )
        .route(
            "/api/v1/admin/sites/{site_id}/secret/rotate",
            axum::routing::post(rotate_secret_handler),
        )
        .route(
            "/api/v1/admin/sites/{site_id}/secret",
            axum::routing::delete(revoke_secret_handler),
        )
        .route(
            "/api/v1/admin/sites/{site_id}/config-snippet",
            axum::routing::get(config_snippet_handler),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));

    Router::new()
        .merge(comment_router)
        .merge(public_router)
        .merge(admin_router)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use crate::request::PaginationQuery;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use cumments_core::models::AuthorType;
    use cumments_core::models::{Comment, CommentAuthor};
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
    fn comment_serializes_nested_author_and_hides_internal_fields() {
        let comment = Comment {
            event_id: "$e:hs".to_string(),
            site_id: "my-blog".to_string(),
            post_slug: "hello".to_string(),
            author: CommentAuthor {
                kind: AuthorType::Guest,
                display_name: Some("Alice".to_string()),
                public_key: Some("pk".to_string()),
                mxid: None,
            },
            content: "hi".to_string(),
            timestamp: chrono::Utc::now(),
            reply_to: None,
            room_id: "!room:hs".to_string(),
            sender_mxid: "@_cumments_my-blog_abcd:hs".to_string(),
        };

        let json = serde_json::to_value(&comment).expect("serialize comment");
        assert_eq!(json["author"]["type"], "guest");
        assert_eq!(json["author"]["public_key"], "pk");
        assert!(json.get("sender_mxid").is_none());
        assert!(json.get("room_id").is_none());
    }
}

use crate::routes::comments::{
    QUERY_METHOD, delete_comment_handler, post_comment_handler, query_comments_handler,
    update_comment_handler,
};
use crate::routes::misc::{get_challenge_handler, health_handler};
use crate::routes::sse::sse_handler;
use axum::{
    Router,
    http::{HeaderName, HeaderValue, Method},
    routing::{delete, get, post},
};
use cumments_core::{
    ports::{CommentStore, IntentStore, SiteStore},
    projector_events::ProjectorEvent,
};
use std::sync::Arc;
use tokio::sync::{Notify, broadcast};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub mod error;
pub mod pow;
pub mod request;
pub mod routes;

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
                displayname: Some("Alice".to_string()),
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

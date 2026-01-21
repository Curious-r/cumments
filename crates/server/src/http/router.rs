use super::handlers::{challenge, comments, sse};
use crate::state::AppState;
use axum::{
    http::{HeaderValue, Method},
    routing::{get, post},
    Router,
};
use tower_http::cors::{Any, CorsLayer};

pub fn build_router(state: AppState, allowed_origins: &str) -> Router {
    let cors = if allowed_origins == "*" {
        CorsLayer::new()
            .allow_methods([Method::GET, Method::POST])
            .allow_origin(Any)
            .allow_headers(Any)
    } else {
        let origins: Vec<HeaderValue> = allowed_origins
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<HeaderValue>().ok())
            .collect();

        if origins.is_empty() {
            tracing::warn!("CORS config is invalid or empty, falling back to allow ANY.");
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST])
                .allow_origin(Any)
                .allow_headers(Any)
        } else {
            tracing::info!("CORS enabled for origins: {:?}", origins);
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST])
                .allow_origin(origins)
                .allow_headers(Any)
        }
    };

    Router::new()
        .route("/api/:site_id/comments/:slug", get(comments::list_comments))
        .route("/api/:site_id/comments", post(comments::post_comment))
        .route("/api/:site_id/comments/:slug/sse", get(sse::sse_handler))
        .route("/api/challenge", get(challenge::get_challenge))
        .layer(cors)
        .with_state(state)
}

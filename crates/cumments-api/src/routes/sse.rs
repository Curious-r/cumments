//! SSE streaming handler for live comment updates.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use axum::{
    extract::{ConnectInfo, Path, State},
    http::HeaderMap,
    response::IntoResponse,
    response::sse::{Event, KeepAlive, Sse},
};
use cumments_core::projector_events::ProjectorEvent;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Decrements the global SSE connection counter when the stream ends.
struct SseConnectionGuard(Arc<AtomicUsize>);

impl Drop for SseConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) async fn sse_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path((site_id, post_slug)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.sse_limiter.allow(&key) {
        return Err(AppError::TooManyRequests(
            "SSE connections are rate limited; try again later".to_string(),
        ));
    }
    if state.active_sse_connections.load(Ordering::Relaxed) >= state.max_sse_connections {
        return Err(AppError::TooManyRequests(
            "too many concurrent SSE connections; try again later".to_string(),
        ));
    }
    state.active_sse_connections.fetch_add(1, Ordering::Relaxed);
    let guard = SseConnectionGuard(state.active_sse_connections.clone());

    let mut rx = state.event_bus.subscribe();

    let stream = async_stream::stream! {
        let _guard = guard;
        while let Ok(event) = rx.recv().await {
            // Filter events by site_id and post_slug
            let matches = match &event {
                ProjectorEvent::CommentCreated { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
                ProjectorEvent::CommentUpdated { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
                ProjectorEvent::CommentDeleted { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
            };

            if matches && let Ok(json) = serde_json::to_string(&event) {
                let event_name = match &event {
                    ProjectorEvent::CommentCreated { .. } => "comment_created",
                    ProjectorEvent::CommentUpdated { .. } => "comment_updated",
                    ProjectorEvent::CommentDeleted { .. } => "comment_deleted",
                };
                yield Ok::<Event, Infallible>(Event::default().event(event_name).data(json));
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

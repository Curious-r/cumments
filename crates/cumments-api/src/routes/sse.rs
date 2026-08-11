//! SSE streaming handler for live comment updates.

use crate::ApiState;
use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
};
use cumments_core::projector_events::ProjectorEvent;
use std::convert::Infallible;

pub(crate) async fn sse_handler(
    State(state): State<ApiState>,
    Path((site_id, post_slug)): Path<(String, String)>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.event_bus.subscribe();

    let stream = async_stream::stream! {
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
                yield Ok(Event::default().event(event_name).data(json));
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

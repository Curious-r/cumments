use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
};
use domain::IngestEvent;
use futures::stream::Stream;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use crate::state::AppState;
pub async fn sse_handler(
    State(state): State<AppState>,
    Path((site_id_str, slug)): Path<(String, String)>,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    let rx = state.tx_ingest.subscribe();
    tracing::info!("SSE Connected: site={} slug={}", site_id_str, slug);
    let stream = BroadcastStream::new(rx).filter_map(move |result| match result {
        Ok(event) => match event {
            IngestEvent::CommentSaved {
                site_id: event_site_id,
                post_slug: event_slug,
                comment,
            } => {
                if event_site_id.as_str() == site_id_str && event_slug == slug {
                    let event_type = if comment.updated_at.is_some() {
                        "update_comment"
                    } else {
                        "new_comment"
                    };
                    Some(
                        Event::default()
                            .event(event_type)
                            .json_data(comment)
                            .map_err(|e| {
                                tracing::error!("SSE serialization error: {}", e);
                                axum::Error::new(e)
                            }),
                    )
                } else {
                    None
                }
            }
            IngestEvent::CommentDeleted {
                site_id: event_site_id,
                post_slug: event_slug,
                comment_id,
            } => {
                if event_site_id.as_str() == site_id_str && event_slug == slug {
                    Some(
                        Event::default()
                            .event("delete_comment")
                            .json_data(serde_json::json!({ "id": comment_id }))
                            .map_err(|e| {
                                tracing::error!("SSE serialization error: {}", e);
                                axum::Error::new(e)
                            }),
                    )
                } else {
                    None
                }
            }
        },
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}

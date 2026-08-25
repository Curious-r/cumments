//! Publishes durable projector events after their local facts have committed.

use cumments_core::ports::SseOutboxStore;
use tokio::sync::broadcast;
use tracing::warn;

/// Publishes pending outbox rows in commit order. A crash between broadcast
/// and deletion can repeat a frame; SSE IDs let consumers discard it.
pub async fn publish_pending(
    store: &dyn SseOutboxStore,
    event_bus: &broadcast::Sender<cumments_core::projector_events::ProjectorEvent>,
) -> anyhow::Result<usize> {
    let rows = store.pending_sse_outbox(100).await?;
    for row in &rows {
        let Some(payload) = row.payload_json.as_deref() else {
            continue;
        };
        let events: Vec<cumments_core::projector_events::ProjectorEvent> =
            serde_json::from_str(payload)?;
        for event in events {
            let _ = event_bus.send(event.clone());
        }
        if let Err(error) = store.mark_sse_outbox_sent(row.id).await {
            warn!(outbox_id = row.id, error = %error, "Failed to delete sent SSE outbox row");
        }
    }
    Ok(rows.len())
}

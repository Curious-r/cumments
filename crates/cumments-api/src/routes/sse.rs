//! SSE streaming handler for live comment updates.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use crate::routes::media::media_url_base;
use axum::{
    extract::{ConnectInfo, Path, State},
    http::HeaderMap,
    response::IntoResponse,
    response::sse::{Event, KeepAlive, Sse},
};
use cumments_core::ephemeral::EphemeralEvent;
use cumments_core::models::{PostSlug, SiteId};
use cumments_core::projector_events::ProjectorEvent;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long after a disconnect a new SSE connection is treated as a reconnect
/// instead of consuming the hourly new-connection budget.
const SSE_RECONNECT_GRACE: Duration = Duration::from_secs(30);
/// Maximum free reconnects per client per rolling window before new
/// connections start consuming the hourly budget again.
const SSE_MAX_FREE_RECONNECTS_PER_WINDOW: u32 = 20;
/// Rolling window for the free-reconnect counter.
const SSE_RECONNECT_WINDOW: Duration = Duration::from_secs(300);
/// Conservative `Retry-After` for the global concurrent-connection cap. The
/// cap is not time-windowed, so a short fixed backoff is the best estimate.
const CONCURRENT_SSE_RETRY_AFTER_SECONDS: u64 = 60;

/// Per-client reconnect bookkeeping so EventSource auto-reconnects and page
/// refreshes do not silently exhaust the SSE connection budget.
#[derive(Default)]
pub struct SseReconnectRegistry {
    entries: HashMap<String, SseReconnectEntry>,
}

#[derive(Default)]
struct SseReconnectEntry {
    last_disconnect: Option<Instant>,
    free_reconnects: u32,
    window_start: Option<Instant>,
}

impl SseReconnectRegistry {
    /// Whether a new connection for `key` may skip the hourly limiter because
    /// it is a recent reconnect. Records the free reconnect when allowed.
    pub fn allow_reconnect(&mut self, key: &str, now: Instant) -> bool {
        let entry = self.entries.entry(key.to_string()).or_default();
        if entry
            .window_start
            .is_none_or(|start| now.duration_since(start) >= SSE_RECONNECT_WINDOW)
        {
            entry.window_start = Some(now);
            entry.free_reconnects = 0;
        }
        let within_grace = entry
            .last_disconnect
            .is_some_and(|last| now.duration_since(last) <= SSE_RECONNECT_GRACE);
        if !within_grace || entry.free_reconnects >= SSE_MAX_FREE_RECONNECTS_PER_WINDOW {
            return false;
        }
        entry.free_reconnects += 1;
        true
    }

    /// Records that a stream for `key` ended, making a subsequent connection
    /// eligible for the reconnect grace.
    pub fn record_disconnect(&mut self, key: &str, now: Instant) {
        let entry = self.entries.entry(key.to_string()).or_default();
        entry.last_disconnect = Some(now);
        self.prune(now);
    }

    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            entry
                .last_disconnect
                .is_some_and(|last| now.duration_since(last) <= SSE_RECONNECT_WINDOW)
        });
    }
}

/// Decrements the global SSE connection counter when the stream ends.
struct SseConnectionGuard {
    active: Arc<AtomicUsize>,
    reconnect: Arc<Mutex<SseReconnectRegistry>>,
    key: String,
}

impl Drop for SseConnectionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
        self.reconnect
            .lock()
            .expect("sse reconnect registry mutex poisoned")
            .record_disconnect(&self.key, Instant::now());
    }
}

pub(crate) async fn sse_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path((site_id, post_slug)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    let now = Instant::now();
    let counted = state.sse_limiter.allow(&key);
    let allowed = counted
        || state
            .sse_reconnect
            .lock()
            .expect("sse reconnect registry mutex poisoned")
            .allow_reconnect(&key, now);
    if !allowed {
        return Err(AppError::TooManyRequests {
            detail: "SSE connections are rate limited; try again later".to_string(),
            retry_after_seconds: state.sse_limiter.window().as_secs(),
        });
    }
    // Validate the path parameters before touching the connection counter so
    // a rejected request can never leak a permanent +1 on the global budget
    // (the guard is created only after validation succeeds).
    let site_id_val = SiteId::new(site_id.clone()).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(post_slug.clone()).map_err(AppError::Validation)?;
    if state.active_sse_connections.load(Ordering::Relaxed) >= state.max_sse_connections {
        return Err(AppError::TooManyRequests {
            detail: "too many concurrent SSE connections; try again later".to_string(),
            retry_after_seconds: CONCURRENT_SSE_RETRY_AFTER_SECONDS,
        });
    }
    state.active_sse_connections.fetch_add(1, Ordering::Relaxed);
    let ephemeral_room_id = state
        .store
        .get_registered_room(&site_id_val, &post_slug_val)
        .await
        .ok()
        .flatten();
    let guard = SseConnectionGuard {
        active: state.active_sse_connections.clone(),
        reconnect: state.sse_reconnect.clone(),
        key,
    };
    let store = state.store.clone();
    let media_proxy = state.media_proxy.clone();
    let media_base = media_url_base(&state, &headers, Some(addr));
    let ephemeral_state = state.ephemeral_state.clone();

    let mut rx = state.event_bus.subscribe();
    let mut ephemeral_rx = state.ephemeral_bus.subscribe();

    let stream = async_stream::stream! {
        let _guard = guard;
        // Initial typing snapshot for this room.
        if let Some(room_id) = &ephemeral_room_id
            && let Some(state) = &ephemeral_state
        {
            for user_id in state.typing_snapshot(room_id) {
                let display_name = match store.get_member(room_id, &user_id).await {
                    Ok(Some(member)) => member.display_name,
                    _ => None,
                };
                let snapshot = serde_json::json!({
                    "type": "typing",
                    "room_id": room_id,
                    "user_id": user_id,
                    "typing": true,
                    "display_name": display_name,
                });
                yield Ok::<Event, Infallible>(Event::default().event("ephemeral").data(snapshot.to_string()));
            }
        }

        enum Incoming {
            Projector(Box<ProjectorEvent>),
            Ephemeral(EphemeralEvent),
        }

        loop {
            let incoming = tokio::select! {
                res = rx.recv() => match res {
                    Ok(event) => Incoming::Projector(Box::new(event)),
                    Err(_) => break,
                },
                res = ephemeral_rx.recv() => match res {
                    Ok(event) => Incoming::Ephemeral(event),
                    Err(_) => break,
                },
            };

            match incoming {
                Incoming::Projector(event) => {
                    let event = *event;
                    // Filter events by site_id and post_slug
                    let matches = match &event {
                        ProjectorEvent::MessageCreated { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
                        ProjectorEvent::MessageUpdated { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
                        ProjectorEvent::MessageAnnotationsChanged { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
                        ProjectorEvent::MessageDeleted { site_id: s, post_slug: p, .. } => s == &site_id && p == &post_slug,
                    };

                    if matches {
                        let mut payload = event.clone();
                        if let Some(proxy) = &media_proxy {
                            match &mut payload {
                                ProjectorEvent::MessageCreated { message, .. }
                                | ProjectorEvent::MessageUpdated { message, .. }
                                | ProjectorEvent::MessageAnnotationsChanged { message, .. } => {
                                    proxy.proxify_message(message, &media_base);
                                }
                                ProjectorEvent::MessageDeleted { .. } => {}
                            }
                        }
                        let Ok(json) = serde_json::to_string(&payload) else {
                            continue;
                        };
                        let event_name = match &event {
                            ProjectorEvent::MessageCreated { .. } => "message_created",
                            ProjectorEvent::MessageUpdated { .. } => "message_updated",
                            ProjectorEvent::MessageAnnotationsChanged { .. } => {
                                "message_annotations_changed"
                            }
                            ProjectorEvent::MessageDeleted { .. } => "message_deleted",
                        };
                        yield Ok::<Event, Infallible>(Event::default().event(event_name).data(json));
                    }
                }
                Incoming::Ephemeral(event) => {
                    match &event {
                        EphemeralEvent::Typing { room_id, .. }
                        | EphemeralEvent::ReadReceipt { room_id, .. } => {
                            if ephemeral_room_id.as_deref() != Some(room_id.as_str()) {
                                continue;
                            }
                        }
                        EphemeralEvent::Presence { user_id, .. } => {
                            // Presence is user-scoped and has no room_id, so
                            // only forward it when the user is a known member
                            // of the subscribed room. Unknown members are
                            // skipped conservatively (membership is projected
                            // from live pushes or backfill).
                            let Some(room_id) = ephemeral_room_id.as_deref() else {
                                continue;
                            };
                            match store.get_member(room_id, user_id).await {
                                Ok(Some(member)) if member.membership == "join" => {}
                                _ => continue,
                            }
                        }
                    }
                    let Ok(json) = serde_json::to_string(&event) else {
                        continue;
                    };
                    yield Ok::<Event, Infallible>(Event::default().event("ephemeral").data(json));
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_grace_allows_recent_reconnect_but_not_stale_ones() {
        let mut registry = SseReconnectRegistry::default();
        let t0 = Instant::now();
        registry.record_disconnect("client", t0);
        assert!(registry.allow_reconnect("client", t0 + Duration::from_secs(1)));
        assert!(!registry.allow_reconnect("client", t0 + Duration::from_secs(31)));
    }

    #[test]
    fn reconnect_free_slots_are_bounded_and_reset_per_window() {
        let mut registry = SseReconnectRegistry::default();
        let t0 = Instant::now();
        registry.record_disconnect("client", t0);
        for i in 0..SSE_MAX_FREE_RECONNECTS_PER_WINDOW {
            assert!(
                registry.allow_reconnect("client", t0 + Duration::from_millis(i as u64 + 1)),
                "free reconnect slot {i}"
            );
        }
        assert!(!registry.allow_reconnect("client", t0 + Duration::from_secs(2)));

        let t1 = t0 + SSE_RECONNECT_WINDOW + Duration::from_secs(1);
        registry.record_disconnect("client", t1);
        assert!(registry.allow_reconnect("client", t1 + Duration::from_secs(1)));
    }
}

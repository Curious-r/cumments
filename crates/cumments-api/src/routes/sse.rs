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
use cumments_core::models::{PageSlug, SiteId};
use cumments_core::projector_events::ProjectorEvent;
use sha2::{Digest, Sha256};
use std::convert::Infallible;

/// Conservative `Retry-After` for the global concurrent-connection cap. The
/// cap is not time-windowed, so a short fixed backoff is the best estimate.
const CONCURRENT_SSE_RETRY_AFTER_SECONDS: u64 = 60;

fn retry_after_seconds(wait: std::time::Duration) -> u64 {
    wait.as_secs().max(1)
}

pub(crate) async fn sse_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path((site_id, page_slug)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    // Reject malformed paths before they consume a limiter token or database
    // query. The limiter runs before resource checks so unregistered pages are
    // still subject to the same anti-flood budget as real pages.
    let site_id_val = SiteId::new(site_id.clone()).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug.clone()).map_err(AppError::Validation)?;
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if let Err(wait) = state.sse_limiter.acquire(&key) {
        return Err(AppError::TooManyRequests {
            detail: "SSE connections are rate limited; try again later".to_string(),
            retry_after_seconds: retry_after_seconds(wait),
        });
    }

    if state
        .store
        .get_site(&site_id_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up site: {e}")))?
        .is_none()
    {
        return Err(AppError::NotFound("Site not found.".to_string()));
    }
    let Some(ephemeral_room_id) = state
        .store
        .get_registered_room(&site_id_val, &page_slug_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve room: {e}")))?
    else {
        return Err(AppError::NotFound(
            "No room registered for this post.".to_string(),
        ));
    };

    // An owned permit moves into the stream and is released automatically when
    // the response body is dropped. This is exact under concurrent requests,
    // unlike check-then-increment on a shared counter.
    let permit = state
        .sse_semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::TooManyRequests {
            detail: "too many concurrent SSE connections; try again later".to_string(),
            retry_after_seconds: CONCURRENT_SSE_RETRY_AFTER_SECONDS,
        })?;
    let store = state.store.clone();
    let media_proxy = state.media_proxy.clone();
    let media_base = media_url_base(&state, &headers, Some(addr));
    let ephemeral_state = state.ephemeral_state.clone();

    let mut rx = state.event_bus.subscribe();
    let mut ephemeral_rx = state.ephemeral_bus.subscribe();

    let stream = async_stream::stream! {
        let _permit = permit;
        // Initial typing snapshot for this room.
        if let Some(state) = &ephemeral_state {
            for user_id in state.typing_snapshot(&ephemeral_room_id) {
                let display_name = match store.get_member(&ephemeral_room_id, &user_id).await {
                    Ok(Some(member)) => member.display_name,
                    _ => None,
                };
                let snapshot = serde_json::json!({
                    "type": "typing",
                    "room_id": ephemeral_room_id,
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
                    // Filter events by site_id and page_slug
                    let matches = match &event {
                        ProjectorEvent::MessageCreated { site_id: s, page_slug: p, .. } => s == &site_id && p == &page_slug,
                        ProjectorEvent::MessageUpdated { site_id: s, page_slug: p, .. } => s == &site_id && p == &page_slug,
                        ProjectorEvent::MessageAnnotationsChanged { site_id: s, page_slug: p, .. } => s == &site_id && p == &page_slug,
                        ProjectorEvent::MessageDeleted { site_id: s, page_slug: p, .. } => s == &site_id && p == &page_slug,
                    };

                    if matches {
                        let mut payload = event.clone();
                        // Live author profile: overlay the current joined
                        // member state so old comments follow display-name
                        // and avatar changes (visitor-identity design §7).
                        match &mut payload {
                            ProjectorEvent::MessageCreated { message, .. }
                            | ProjectorEvent::MessageUpdated { message, .. }
                            | ProjectorEvent::MessageAnnotationsChanged { message, .. } => {
                                if let Ok(Some(member)) =
                                    store.get_member(&message.room_id, &message.sender_mxid).await
                                    && member.membership == "join"
                                {
                                    message.author.display_name = member.display_name;
                                    message.author.avatar_url = member.avatar_url;
                                }
                            }
                            ProjectorEvent::MessageDeleted { .. } => {}
                        }
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
                        let mut hasher = Sha256::new();
                        hasher.update(json.as_bytes());
                        let id = hex::encode(hasher.finalize());
                        yield Ok::<Event, Infallible>(
                            Event::default()
                                .event(event_name)
                                .id(id)
                                .data(json),
                        );
                    }
                }
                Incoming::Ephemeral(event) => {
                    match &event {
                        EphemeralEvent::Typing { room_id, .. }
                        | EphemeralEvent::ReadReceipt { room_id, .. } => {
                            if room_id.as_str() != ephemeral_room_id {
                                continue;
                            }
                        }
                        EphemeralEvent::Presence { user_id, .. } => {
                            // Presence is user-scoped and has no room_id, so
                            // only forward it when the user is a known member
                            // of the subscribed room. Unknown members are
                            // skipped conservatively (membership is projected
                            // from live pushes or backfill).
                            match store.get_member(&ephemeral_room_id, user_id).await {
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

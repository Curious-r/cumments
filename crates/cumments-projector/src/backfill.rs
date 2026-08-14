//! Backfill: rebuild the read model from Matrix room history.
//!
//! Since ownership is publicly verifiable (Ed25519 keys in events), a room's
//! history is sufficient to rebuild its comments completely. The backfiller:
//!
//! 1. discovers Cumments rooms via `get_joined_rooms` + room metadata (works even
//!    after a full local DB reset, rebuilding sites and the room registry),
//! 2. fetches each comment room's history page by page (`/messages`, newest
//!    first), persisting a pagination cursor for interrupted runs,
//! 3. sorts events by (origin_server_ts, event_id) so edits/redactions are
//!    replayed in order, then feeds them through the same transport-agnostic
//!    projection as live push events (idempotent upserts).

use cumments_core::models::{PostSlug, SiteId};
use cumments_core::ports::{BackfillCursorStore, MatrixDriver, RegistryStore, SiteStore};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::event_processor::EventProcessor;
use crate::parsed::parse_room_identity;
use crate::push_receiver::{PushEvent, process_single_event};

const PAGE_SIZE: u32 = 100;
const PAGE_DELAY: Duration = Duration::from_millis(50);
/// Hard upper bound on events buffered per room before chronological replay.
///
/// Replay must see edits/redactions after their targets, so fetched events are
/// buffered and sorted. Without a cap an unlimited `--max-pages 0` run on a
/// very large room could exhaust memory; when the cap is hit the room fails
/// loudly instead and the operator reruns with a smaller `--max-pages`. The
/// cursor is not advanced on this failure, so nothing is skipped.
const MAX_BUFFERED_EVENTS: usize = 20_000;

pub struct Backfiller {
    driver: Arc<dyn MatrixDriver>,
    processor: Arc<EventProcessor>,
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
    cursor_store: Arc<dyn BackfillCursorStore>,
}

#[derive(Debug, Default)]
pub struct BackfillSummary {
    pub rooms: usize,
    pub events: usize,
}

/// A joined room that participates in backfill.
enum DiscoveredRoom {
    /// A comment room; backfilled to rebuild messages and room roles.
    Comment(String),
    /// A site Space; backfilled to rebuild site roles (and space state).
    Space(String),
}

impl Backfiller {
    pub fn new(
        driver: Arc<dyn MatrixDriver>,
        processor: Arc<EventProcessor>,
        site_store: Arc<dyn SiteStore>,
        registry_store: Arc<dyn RegistryStore>,
        cursor_store: Arc<dyn BackfillCursorStore>,
    ) -> Self {
        Self {
            driver,
            processor,
            site_store,
            registry_store,
            cursor_store,
        }
    }

    /// Discover Cumments rooms, rebuild site/registry entries, then backfill
    /// every comment room. A failure in one room is logged and skipped so a
    /// single broken room does not abort the whole run.
    pub async fn run(&self, max_pages: u32) -> anyhow::Result<BackfillSummary> {
        let mut comment_rooms = Vec::new();
        let mut space_rooms = Vec::new();

        for room_id in self.driver.get_joined_rooms().await? {
            match self.discover_room(&room_id).await {
                Ok(Some(DiscoveredRoom::Comment(room_id))) => comment_rooms.push(room_id),
                Ok(Some(DiscoveredRoom::Space(room_id))) => space_rooms.push(room_id),
                Ok(None) => {}
                Err(e) => warn!(
                    "Backfill: discovery failed for {}: {:#}; skipping",
                    room_id, e
                ),
            }
        }

        let mut summary = BackfillSummary {
            rooms: comment_rooms.len() + space_rooms.len(),
            ..Default::default()
        };
        for room_id in comment_rooms.into_iter().chain(space_rooms) {
            // 0 means "unlimited" (u32::MAX is effectively unbounded).
            let room_max_pages = if max_pages == 0 { u32::MAX } else { max_pages };
            match self.backfill_room(&room_id, room_max_pages).await {
                Ok(events) => {
                    summary.events += events;
                    info!("Backfilled {} ({} events)", room_id, events);
                }
                Err(e) => warn!("Backfill failed for {}: {:?}", room_id, e),
            }
        }
        Ok(summary)
    }

    /// Rebuild the site/registry entries for one joined room. Returns a
    /// `Comment` room (messages + room roles), a `Space` room (site roles
    /// and space state), or `None` for non-Cumments rooms.
    ///
    /// Errors are returned per room so the caller can log and continue; one
    /// broken room must not abort the whole discovery pass.
    async fn discover_room(&self, room_id: &str) -> anyhow::Result<Option<DiscoveredRoom>> {
        let meta = match self.driver.get_room_metadata(room_id).await {
            Ok(meta) => meta,
            Err(e) => {
                warn!(
                    "Backfill: failed to read metadata for {}: {:?}; skipping",
                    room_id, e
                );
                return Ok(None);
            }
        };
        let Some(meta) = meta else {
            // No metadata: legacy rooms (created before the metadata state
            // event existed) can still be identified from their canonical
            // alias, which lives in our exclusive `#_cumments_*` namespace.
            if self.register_legacy_room_by_alias(room_id).await? {
                return Ok(Some(DiscoveredRoom::Comment(room_id.to_string())));
            }
            return Ok(None);
        };
        let Some(site_id) = meta.get("site_id").and_then(|v| v.as_str()) else {
            if self.register_legacy_room_by_alias(room_id).await? {
                return Ok(Some(DiscoveredRoom::Comment(room_id.to_string())));
            }
            return Ok(None);
        };

        match meta.get("post_slug").and_then(|v| v.as_str()) {
            // Space room: restores the site -> space mapping.
            None => match SiteId::new(site_id.to_owned()) {
                Ok(site_id_val) => {
                    self.site_store
                        .ensure_site_exists(site_id_val.as_str(), room_id)
                        .await?;
                    info!(
                        "Backfill: registered site {} (space {})",
                        site_id_val.as_str(),
                        room_id
                    );
                    return Ok(Some(DiscoveredRoom::Space(room_id.to_string())));
                }
                Err(_) => warn!(
                    "Backfill: skipping space {} with invalid site id {}",
                    room_id, site_id
                ),
            },
            // Comment room: restore registry entry and backfill it.
            Some(post_slug) => {
                match (
                    SiteId::new(site_id.to_owned()),
                    PostSlug::new(post_slug.to_owned()),
                ) {
                    (Ok(site_id_val), Ok(post_slug_val)) => {
                        self.registry_store
                            .register_room_if_absent(room_id, &site_id_val, &post_slug_val)
                            .await?;
                        info!(
                            "Backfill: discovered comment room {} for {}/{}",
                            room_id,
                            site_id_val.as_str(),
                            post_slug_val.as_str()
                        );
                        return Ok(Some(DiscoveredRoom::Comment(room_id.to_string())));
                    }
                    _ => warn!(
                        "Backfill: skipping room {} with invalid identity {}/{}",
                        room_id, site_id, post_slug
                    ),
                }
            }
        }
        Ok(None)
    }

    /// Try to identify a metadata-less room from its canonical alias and
    /// register it as a comment room. Returns `true` when the room was
    /// registered. Read-only with respect to Matrix.
    async fn register_legacy_room_by_alias(&self, room_id: &str) -> anyhow::Result<bool> {
        let alias = match self.driver.get_room_canonical_alias(room_id).await {
            Ok(alias) => alias,
            Err(e) => {
                warn!(
                    "Backfill: failed to read canonical alias for {}: {:?}; skipping",
                    room_id, e
                );
                return Ok(false);
            }
        };
        let Some(alias) = alias else {
            debug!(
                "Backfill: skipping room {} without metadata or canonical alias",
                room_id
            );
            return Ok(false);
        };

        let Some(identity) = parse_room_identity(None, Some(&alias)) else {
            debug!(
                "Backfill: skipping room {} with non-Cumments alias {}",
                room_id, alias
            );
            return Ok(false);
        };
        let Ok(site_id) = SiteId::new(identity.site_id) else {
            warn!(
                "Backfill: skipping room {} with invalid site id from alias {}",
                room_id, alias
            );
            return Ok(false);
        };
        let Ok(post_slug) = PostSlug::new(identity.post_slug) else {
            warn!(
                "Backfill: skipping room {} with invalid post slug from alias {}",
                room_id, alias
            );
            return Ok(false);
        };

        self.registry_store
            .register_room_if_absent(room_id, &site_id, &post_slug)
            .await?;
        info!(
            "Backfill: discovered legacy comment room {} for {}/{} via alias {}",
            room_id,
            site_id.as_str(),
            post_slug.as_str(),
            alias
        );
        Ok(true)
    }

    /// Fetch a room's full history (or continue from the stored cursor),
    /// replay it in chronological order, and persist the next cursor.
    async fn backfill_room(&self, room_id: &str, max_pages: u32) -> anyhow::Result<usize> {
        let mut from = self.cursor_store.get_cursor(room_id).await?;
        let mut collected: Vec<serde_json::Value> = Vec::new();
        let mut last_batch: Option<String> = None;
        let mut done = false;
        let mut pages = 0u32;

        loop {
            if pages >= max_pages {
                warn!(
                    "Backfill reached the {}-page cap for {}; cursor saved, run again to continue",
                    max_pages, room_id
                );
                break;
            }
            let page = self
                .driver
                .get_room_events(room_id, from.as_deref(), PAGE_SIZE)
                .await?;
            pages += 1;
            collected.extend(page.events);
            if collected.len() > MAX_BUFFERED_EVENTS {
                anyhow::bail!(
                    "backfill of room {room_id} exceeds the in-memory buffer cap \
                     ({MAX_BUFFERED_EVENTS} events); rerun with --max-pages to process it \
                     in bounded batches"
                );
            }
            done = !page.has_more;
            match page.next_token {
                Some(next) => {
                    last_batch = Some(next.clone());
                    from = Some(next);
                }
                None => break,
            }
            if done {
                break;
            }
            tokio::time::sleep(PAGE_DELAY).await;
        }

        // Newest-first pagination returns edits/redactions before their
        // targets; replay chronologically so the projection is order-correct.
        sort_events(&mut collected);

        let mut processed = 0usize;
        for event in &collected {
            let Ok(push_event) = serde_json::from_value::<PushEvent>(event.clone()) else {
                continue;
            };
            process_single_event(&push_event, &self.processor).await?;
            processed += 1;
        }

        if !done && let Some(next) = last_batch {
            self.cursor_store.save_cursor(room_id, &next).await?;
        }
        Ok(processed)
    }
}

/// Sort raw room events by (origin_server_ts, event_id) so replay order is
/// deterministic. Events missing timestamps sort last.
fn event_sort_key(event: &serde_json::Value) -> (i64, String) {
    let ts = event
        .get("origin_server_ts")
        .and_then(|v| v.as_i64())
        .unwrap_or(i64::MAX);
    let id = event
        .get("event_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (ts, id)
}

fn sort_events(events: &mut [serde_json::Value]) {
    events.sort_by_key(event_sort_key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_sort_by_timestamp_then_event_id() {
        let mut events = vec![
            json!({"origin_server_ts": 200, "event_id": "$b"}),
            json!({"origin_server_ts": 100, "event_id": "$a"}),
            json!({"origin_server_ts": 200, "event_id": "$a"}),
        ];
        sort_events(&mut events);
        let ids: Vec<_> = events
            .iter()
            .map(|e| e["event_id"].as_str().unwrap())
            .collect();
        // (100, $a) < (200, $a) < (200, $b)
        assert_eq!(ids, vec!["$a", "$a", "$b"]);
    }

    #[test]
    fn events_missing_timestamp_sort_last() {
        let mut events = vec![
            json!({"origin_server_ts": 100, "event_id": "$a"}),
            json!({"type": "m.room.message"}),
            json!({"origin_server_ts": 50, "event_id": "$z"}),
        ];
        sort_events(&mut events);
        assert_eq!(events[0]["event_id"], "$z");
        assert_eq!(events[1]["event_id"], "$a");
        assert!(events[2].get("event_id").is_none());
    }
}

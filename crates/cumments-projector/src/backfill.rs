//! Backfill: rebuild the read model from Matrix room history.
//!
//! Since ownership is publicly verifiable (Ed25519 keys in events), a room's
//! history is sufficient to rebuild its comments completely. The backfiller:
//!
//! 1. discovers Cumments rooms via `get_joined_rooms` + room metadata (works even
//!    after a full local DB reset, rebuilding sites and the room registry),
//! 2. fetches each comment room's history page by page (`/messages`, newest
//!    first), persisting a pagination cursor for interrupted runs,
//! 3. preserves homeserver stream/topological order across pages so
//!    edits/redactions follow their targets, then feeds events through the
//!    same transport-agnostic projection as live pushes (idempotent upserts).

use cumments_core::models::{PageSlug, SiteId};
use cumments_core::ports::{BackfillCursorStore, MatrixDriver, RegistryStore, SiteStore};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::event_processor::EventProcessor;
use crate::parsed::parse_room_identity;
use crate::push_receiver::{PushEvent, process_single_event};
use tokio::sync::mpsc;

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

/// A bot-triggered backfill request. The worker replies to `reply_room_id`
/// (the private DM) when the run finishes.
#[derive(Debug, Clone)]
pub struct BackfillRequest {
    pub actor_mxid: String,
    pub reply_room_id: String,
    pub max_pages: u32,
}

/// Sequential backfill worker: single-flight by construction (one receiver,
/// one job at a time). Completion or failure is reported as a bot DM.
pub struct BackfillWorker {
    rx: mpsc::Receiver<BackfillRequest>,
    driver: Arc<dyn MatrixDriver>,
    processor: Arc<EventProcessor>,
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
    cursor_store: Arc<dyn BackfillCursorStore>,
}

impl BackfillWorker {
    pub fn new(
        rx: mpsc::Receiver<BackfillRequest>,
        driver: Arc<dyn MatrixDriver>,
        processor: Arc<EventProcessor>,
        site_store: Arc<dyn SiteStore>,
        registry_store: Arc<dyn RegistryStore>,
        cursor_store: Arc<dyn BackfillCursorStore>,
    ) -> Self {
        Self {
            rx,
            driver,
            processor,
            site_store,
            registry_store,
            cursor_store,
        }
    }

    pub async fn run(mut self) {
        while let Some(request) = self.rx.recv().await {
            let backfiller = Backfiller::new(
                self.driver.clone(),
                self.processor.clone(),
                self.site_store.clone(),
                self.registry_store.clone(),
                self.cursor_store.clone(),
            );
            let message = match backfiller.run(request.max_pages).await {
                Ok(summary) => format!(
                    "backfill 完成：{} 个房间 / {} 个事件",
                    summary.rooms, summary.events
                ),
                Err(error) => format!("backfill 失败：{:#}", error),
            };
            if let Err(error) = self
                .driver
                .send_bot_message(&request.reply_room_id, &message)
                .await
            {
                warn!(
                    "backfill completion DM to {} failed: {:#}",
                    request.actor_mxid, error
                );
            }
        }
    }
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

        match meta.get("page_slug").and_then(|v| v.as_str()) {
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
            Some(page_slug) => {
                match (
                    SiteId::new(site_id.to_owned()),
                    PageSlug::new(page_slug.to_owned()),
                ) {
                    (Ok(site_id_val), Ok(page_slug_val)) => {
                        self.registry_store
                            .register_room_if_absent(room_id, &site_id_val, &page_slug_val)
                            .await?;
                        info!(
                            "Backfill: discovered comment room {} for {}/{}",
                            room_id,
                            site_id_val.as_str(),
                            page_slug_val.as_str()
                        );
                        return Ok(Some(DiscoveredRoom::Comment(room_id.to_string())));
                    }
                    _ => warn!(
                        "Backfill: skipping room {} with invalid identity {}/{}",
                        room_id, site_id, page_slug
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
        let Ok(page_slug) = PageSlug::new(identity.page_slug) else {
            warn!(
                "Backfill: skipping room {} with invalid page slug from alias {}",
                room_id, alias
            );
            return Ok(false);
        };

        self.registry_store
            .register_room_if_absent(room_id, &site_id, &page_slug)
            .await?;
        info!(
            "Backfill: discovered legacy comment room {} for {}/{} via alias {}",
            room_id,
            site_id.as_str(),
            page_slug.as_str(),
            alias
        );
        Ok(true)
    }

    /// Fetch a room's full history (or continue from the stored cursor),
    /// replay it in chronological order, and persist the next cursor.
    async fn backfill_room(&self, room_id: &str, max_pages: u32) -> anyhow::Result<usize> {
        let mut from = self.cursor_store.get_cursor(room_id).await?;
        // Pages arrive newest-first and preserve homeserver stream/topological
        // order within the requested direction. Keep pages in fetch order,
        // then reverse the complete buffer once for chronological replay.
        let mut ordered_events: Vec<serde_json::Value> = Vec::new();
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
            let mut oldest_first = page.events;
            oldest_first.reverse();
            ordered_events.extend(oldest_first);
            if ordered_events.len() > MAX_BUFFERED_EVENTS {
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

        let mut processed = 0usize;
        ordered_events.reverse();
        for event in &ordered_events {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn backward_pages_are_stitched_in_homeserver_order() {
        let mut fetched_newest_first = [
            json!({"event_id": "$newest"}),
            json!({"event_id": "$middle"}),
            json!({"event_id": "$oldest"}),
        ];
        fetched_newest_first.reverse();

        let ids: Vec<_> = fetched_newest_first
            .iter()
            .map(|event| event["event_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["$oldest", "$middle", "$newest"]);
    }
}

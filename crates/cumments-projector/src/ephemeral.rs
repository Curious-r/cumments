//! Long-lived `/sync` ingestion for ephemeral room events (typing, public
//! read receipts, presence) and the in-memory state backing SSE snapshots.

use anyhow::{Result, anyhow};
use cumments_core::ephemeral::{EphemeralEvent, EphemeralState};
use cumments_core::ports::{RegistryStore, RoomStore};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::warn;

/// The long-lived AppService sync loop that feeds ephemeral events to SSE.
pub struct EphemeralSync {
    homeserver_url: String,
    as_token: String,
    sender_user_id: String,
    http_client: reqwest::Client,
    registry_store: Arc<dyn RegistryStore>,
    room_store: Arc<dyn RoomStore>,
    state: Arc<EphemeralState>,
    bus: broadcast::Sender<EphemeralEvent>,
}

impl EphemeralSync {
    pub fn new(
        homeserver_url: String,
        as_token: String,
        sender_user_id: String,
        registry_store: Arc<dyn RegistryStore>,
        room_store: Arc<dyn RoomStore>,
        state: Arc<EphemeralState>,
        bus: broadcast::Sender<EphemeralEvent>,
    ) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            homeserver_url,
            as_token,
            sender_user_id,
            http_client,
            registry_store,
            room_store,
            state,
            bus,
        })
    }

    /// Runs forever, syncing ephemeral state for all active rooms.
    pub async fn run(&self) -> ! {
        let mut since: Option<String> = None;
        loop {
            let rooms = match self.registry_store.list_active_rooms().await {
                Ok(rooms) => rooms,
                Err(e) => {
                    warn!("ephemeral sync: failed to list rooms: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    continue;
                }
            };
            match self.sync_once(&rooms, since.as_deref()).await {
                Ok(next_batch) => since = Some(next_batch),
                Err(e) => {
                    warn!("ephemeral sync failed: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }
    }

    async fn sync_once(&self, rooms: &[String], since: Option<&str>) -> Result<String> {
        let filter = serde_json::json!({
            "room": {
                "rooms": rooms,
                "timeline": { "limit": 0 },
                "ephemeral": { "typing": true, "receipt": true },
                "state": { "limit": 0 }
            },
            "presence": { "types": ["m.presence"] }
        });
        let filter_json = filter.to_string();
        let mut query = vec![
            ("user_id", self.sender_user_id.as_str()),
            ("timeout", "30000"),
            ("filter", filter_json.as_str()),
        ];
        if let Some(since) = since {
            query.push(("since", since));
        }
        let url = format!(
            "{}/_matrix/client/v3/sync",
            self.homeserver_url.trim_end_matches('/')
        );
        let resp = self
            .http_client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.as_token))
            .query(&query)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("sync failed ({status}): {body}"));
        }
        let body: serde_json::Value = resp.json().await?;
        let next_batch = body
            .get("next_batch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("sync response missing next_batch"))?
            .to_string();

        if let Some(rooms) = body.get("rooms").and_then(|v| v.as_object()) {
            for (room_id, room) in rooms {
                if let Some(events) = room
                    .get("ephemeral")
                    .and_then(|e| e.get("events"))
                    .and_then(|e| e.as_array())
                {
                    for event in events {
                        match event.get("type").and_then(|v| v.as_str()) {
                            Some("m.typing") => {
                                self.handle_typing(room_id, event).await;
                            }
                            Some("m.receipt") => {
                                self.handle_receipts(room_id, event).await;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        if let Some(presence) = body.get("presence").and_then(|p| p.as_array()) {
            for event in presence {
                if event.get("type").and_then(|v| v.as_str()) != Some("m.presence") {
                    continue;
                }
                let Some(user_id) = event.get("sender").and_then(|v| v.as_str()).or_else(|| {
                    event
                        .get("content")
                        .and_then(|c| c.get("user_id"))
                        .and_then(|v| v.as_str())
                }) else {
                    continue;
                };
                let presence = event
                    .get("content")
                    .and_then(|c| c.get("presence"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let _ = self.bus.send(EphemeralEvent::Presence {
                    user_id: user_id.to_string(),
                    presence,
                });
            }
        }
        Ok(next_batch)
    }

    async fn handle_typing(&self, room_id: &str, event: &serde_json::Value) {
        let current: HashSet<String> = event
            .get("content")
            .and_then(|c| c.get("user_ids"))
            .and_then(|u| u.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        let previous: HashSet<String> = self.state.typing_snapshot(room_id).into_iter().collect();

        for user in previous.difference(&current) {
            self.emit_typing(room_id, user, false, None).await;
        }
        for user in current.difference(&previous) {
            let display_name = self
                .room_store
                .get_member(room_id, user)
                .await
                .ok()
                .flatten()
                .and_then(|m| m.display_name);
            self.emit_typing(room_id, user, true, display_name).await;
        }
    }

    async fn emit_typing(
        &self,
        room_id: &str,
        user_id: &str,
        typing: bool,
        display_name: Option<String>,
    ) {
        let event = EphemeralEvent::Typing {
            room_id: room_id.to_string(),
            user_id: user_id.to_string(),
            typing,
            display_name,
        };
        self.state.apply(&event);
        let _ = self.bus.send(event);
    }

    async fn handle_receipts(&self, room_id: &str, event: &serde_json::Value) {
        let Some(content) = event.get("content").and_then(|c| c.as_object()) else {
            return;
        };
        for (event_id, receipts) in content {
            let Some(read) = receipts.get("m.read").and_then(|r| r.as_object()) else {
                continue;
            };
            for user_id in read.keys() {
                let _ = self.bus.send(EphemeralEvent::ReadReceipt {
                    room_id: room_id.to_string(),
                    event_id: event_id.clone(),
                    user_id: user_id.clone(),
                });
            }
        }
    }
}

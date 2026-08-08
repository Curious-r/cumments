//! AppService MatrixDriver – uses `reqwest` to call the Matrix CS API
//! directly with the AppService `as_token`, supporting virtual users.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::{
    identity::derive_visitor_id_from_public_key,
    models::{PostSlug, RoomEventPage, SiteId},
    ports::{MatrixDriver, VirtualUserStore},
};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, instrument, warn};

// ── Response types for Matrix CS API ──────────────────────────────

#[derive(Deserialize)]
struct CreateRoomResponse {
    room_id: String,
}

#[derive(Deserialize)]
struct SendEventResponse {
    event_id: String,
}

#[derive(Deserialize)]
struct ResolveAliasResponse {
    room_id: String,
}

#[derive(Deserialize)]
struct MessagesResponse {
    start: String,
    end: String,
    chunk: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct JoinedRoomsResponse {
    joined_rooms: Vec<String>,
}

/// Whether a metadata state payload matches the expected Cumments identity.
/// Spaces carry `post_slug: null`; comment rooms carry the post slug.
fn metadata_matches(meta: &serde_json::Value, site_id: &str, post_slug: Option<&str>) -> bool {
    let site_ok = meta.get("site_id").and_then(|v| v.as_str()) == Some(site_id);
    let slug_ok = match post_slug {
        Some(slug) => meta.get("post_slug").and_then(|v| v.as_str()) == Some(slug),
        None => matches!(meta.get("post_slug"), None | Some(serde_json::Value::Null)),
    };
    site_ok && slug_ok
}

/// The AppService-based Matrix driver.
///
/// Instead of using a logged-in client session, this driver
/// authenticates with the AppService `as_token` and can impersonate
/// any virtual user in the AppService namespace.
pub struct AppServiceMatrixDriver {
    http_client: reqwest::Client,
    homeserver_url: String,
    as_token: String,
    server_name: String,
    sender_localpart: String,
    owner_id: String,
    virtual_user_store: Arc<dyn VirtualUserStore>,
}

impl AppServiceMatrixDriver {
    pub fn new(
        homeserver_url: String,
        as_token: String,
        server_name: String,
        sender_localpart: String,
        owner_id: String,
        virtual_user_store: Arc<dyn VirtualUserStore>,
    ) -> Self {
        Self {
            http_client: reqwest::Client::new(),
            homeserver_url,
            as_token,
            server_name,
            sender_localpart,
            owner_id,
            virtual_user_store,
        }
    }

    // ── Internal helpers ──────────────────────────────────────────

    /// Build a fully qualified user ID from a localpart.
    fn user_id(&self, localpart: &str) -> String {
        format!("@{}:{}", localpart, self.server_name)
    }

    /// The sender (bot) user ID for this AppService.
    fn sender_user_id(&self) -> String {
        self.user_id(&self.sender_localpart)
    }

    /// Generate a unique transaction ID for idempotent requests.
    fn txn_id(&self) -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("cumments_{}", ts)
    }

    /// Make an authenticated CS API request with optional virtual user.
    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        virtual_user: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let url = format!("{}/{}", self.homeserver_url.trim_end_matches('/'), path);
        let mut req = self
            .http_client
            .request(method, &url)
            .header("Authorization", format!("Bearer {}", self.as_token));
        if let Some(vu) = virtual_user {
            req = req.query(&[("user_id", vu)]);
        }
        req
    }

    /// Resolve the virtual user ID for a given author public key.
    async fn resolve_virtual_user(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<String> {
        self.virtual_user_store
            .get_or_create_virtual_user(author_public_key, site_id, &self.server_name)
            .await
    }

    /// Ensure a virtual user is joined to a room.
    async fn ensure_joined(&self, room_id: &str, virtual_user: &str) -> Result<()> {
        let path = format!("_matrix/client/v3/rooms/{}/join", urlencode(room_id));
        let resp = self
            .request(reqwest::Method::POST, &path, Some(virtual_user))
            .send()
            .await
            .map_err(|e| anyhow!("Join request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                "Virtual user {} join room {} failed ({}): {}",
                virtual_user, room_id, status, body
            );
            // Non-fatal – the user may already be joined (M_FORGE can happen)
            // or the HS auto-joined via AS protocol.
        }
        Ok(())
    }

    /// Set cumments metadata on a room (state event `im.cumments.metadata`).
    async fn set_room_metadata(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: Option<&PostSlug>,
    ) -> Result<()> {
        let content = serde_json::json!({
            "site_id": site_id.as_str(),
            "post_slug": post_slug.map(|s| s.as_str()),
        });
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/im.cumments.metadata",
            urlencode(room_id)
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, None)
            .json(&content)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to set room metadata: {}", e))?;

        if !resp.status().is_success() {
            warn!(
                "Setting metadata for room {} failed: {}",
                room_id,
                resp.status()
            );
        }
        Ok(())
    }

    /// Query a room's metadata state event.
    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/im.cumments.metadata",
            urlencode(room_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to query room metadata: {}", e))?;

        if resp.status().is_success() {
            Ok(Some(resp.json().await?))
        } else {
            Ok(None)
        }
    }

    /// Resolve a room ID from its alias via the homeserver directory.
    async fn resolve_room_by_alias(&self, alias: &str) -> Result<Option<String>> {
        let path = format!("_matrix/client/v3/directory/room/{}", urlencode(alias));
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to resolve room alias {}: {}", alias, e))?;

        if resp.status().is_success() {
            let data: ResolveAliasResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("Failed to parse alias response: {}", e))?;
            Ok(Some(data.room_id))
        } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(None)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Alias lookup {} failed ({}): {}",
                alias,
                status,
                body
            ))
        }
    }

    /// Whether a room's metadata matches the expected identity.
    async fn room_metadata_matches(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: Option<&PostSlug>,
    ) -> Result<bool> {
        Ok(match self.get_room_metadata(room_id).await? {
            Some(meta) => metadata_matches(&meta, site_id.as_str(), post_slug.map(|s| s.as_str())),
            None => false,
        })
    }

    /// Best-effort, idempotent: link a comment room into its site Space
    /// (`m.space.child` on the Space, `m.space.parent` on the room).
    async fn link_room_to_space(&self, space_id: &str, room_id: &str) {
        let child_content = serde_json::json!({ "via": [self.server_name] });
        let space_path = format!(
            "_matrix/client/v3/rooms/{}/state/m.space.child/{}",
            urlencode(space_id),
            urlencode(room_id)
        );
        let _ = self
            .request(reqwest::Method::PUT, &space_path, None)
            .json(&child_content)
            .send()
            .await;

        let parent_path = format!(
            "_matrix/client/v3/rooms/{}/state/m.space.parent/{}",
            urlencode(room_id),
            urlencode(space_id)
        );
        let parent_content = serde_json::json!({
            "via": [self.server_name],
            "canonical": true
        });
        let _ = self
            .request(reqwest::Method::PUT, &parent_path, None)
            .json(&parent_content)
            .send()
            .await;
    }
}

#[async_trait]
impl MatrixDriver for AppServiceMatrixDriver {
    #[instrument(skip(self), fields(site_id = %site_id.as_str()))]
    async fn create_site_space(&self, site_id: &SiteId) -> Result<String> {
        let site_id_str = site_id.as_str();
        let alias_localpart = format!("cumments_{}", site_id_str);

        info!("Creating new space for site {} via AppService", site_id_str);

        let mut body = serde_json::json!({
            "name": format!("Comments: {}", site_id_str),
            "room_alias_name": alias_localpart,
            "creation_content": {
                "room_type": "m.space"
            },
            "initial_state": [
                {
                    "type": "im.cumments.metadata",
                    "state_key": "",
                    "content": {
                        "site_id": site_id_str,
                        "post_slug": null
                    }
                },
                {
                    "type": "m.room.power_levels",
                    "state_key": "",
                    "content": {
                        "users": {}
                    }
                }
            ],
            "invite": [self.owner_id.clone()],
            "preset": "public_chat",
        });

        // Inject power levels with sender and owner as admins
        if let Some(pl) = body.pointer_mut("/initial_state/1/content/users")
            && let Some(obj) = pl.as_object_mut()
        {
            obj.insert(self.sender_user_id(), 100.into());
            obj.insert(self.owner_id.clone(), 100.into());
        }

        let resp = self
            .request(reqwest::Method::POST, "_matrix/client/v3/createRoom", None)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("createRoom request failed: {}", e))?;

        if resp.status().is_success() {
            let data: CreateRoomResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("Failed to parse createRoom response: {}", e))?;
            Ok(data.room_id)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                "createRoom failed ({}): {}. Trying alias recovery.",
                status, body
            );

            // Recovery: after a local DB reset (or a partial previous run) the
            // space may already exist under our exclusive alias. Adopt it.
            let alias = format!("#cumments_{}:{}", site_id_str, self.server_name);
            match self.resolve_room_by_alias(&alias).await? {
                Some(room_id) => {
                    if self.room_metadata_matches(&room_id, site_id, None).await? {
                        info!(
                            "Recovered existing site space {} via alias {}",
                            room_id, alias
                        );
                    } else {
                        warn!(
                            "Adopting room {} under alias {} with repaired metadata",
                            room_id, alias
                        );
                        let _ = self.set_room_metadata(&room_id, site_id, None).await;
                    }
                    Ok(room_id)
                }
                None => Err(anyhow!(
                    "Failed to create site space ({}): {}",
                    status,
                    body
                )),
            }
        }
    }

    #[instrument(skip(self), fields(site_id = %site_id.as_str(), post_slug = %post_slug.as_str()))]
    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        space_id: &str,
        candidate_room_id: Option<&str>,
    ) -> Result<String> {
        let mut target_room_id = None;

        // ── PHASE 0: O(1) DISCOVERY (Check Candidate) ──
        if let Some(candidate) = candidate_room_id
            && let Ok(Some(meta)) = self.get_room_metadata(candidate).await
            && metadata_matches(&meta, site_id.as_str(), Some(post_slug.as_str()))
        {
            target_room_id = Some(candidate.to_string());
        }

        // ── PHASE 0.5: ALIAS RECOVERY (cold local registry) ──
        if target_room_id.is_none() {
            let alias = format!(
                "#cumments_{}_{}:{}",
                site_id.as_str(),
                post_slug.as_str(),
                self.server_name
            );
            if let Some(room_id) = self.resolve_room_by_alias(&alias).await? {
                if self
                    .room_metadata_matches(&room_id, site_id, Some(post_slug))
                    .await?
                {
                    info!(
                        "Recovered existing comment room {} via alias {}",
                        room_id, alias
                    );
                } else {
                    // The alias namespace is exclusive to us, so this is a room
                    // we created before metadata was written; repair it.
                    warn!(
                        "Adopting room {} under alias {} with repaired metadata",
                        room_id, alias
                    );
                    let _ = self
                        .set_room_metadata(&room_id, site_id, Some(post_slug))
                        .await;
                }
                target_room_id = Some(room_id);
            }
        }

        let room_id = if let Some(id) = target_room_id {
            id
        } else {
            // ── PHASE 1: Create new comment room ──
            info!("No matching room found. Creating new comment room via AppService.");
            let alias_localpart = format!("cumments_{}_{}", site_id.as_str(), post_slug.as_str());

            let mut body = serde_json::json!({
                "name": format!("Comments: {}/{}", site_id.as_str(), post_slug.as_str()),
                "room_alias_name": alias_localpart,
                "initial_state": [
                    {
                        "type": "im.cumments.metadata",
                        "state_key": "",
                        "content": {
                            "site_id": site_id.as_str(),
                            "post_slug": post_slug.as_str()
                        }
                    },
                    {
                        "type": "m.room.power_levels",
                        "state_key": "",
                        "content": {
                            "users": {}
                        }
                    }
                ],
                "invite": [self.owner_id.clone()],
                "preset": "public_chat",
            });

            // Inject power levels with sender and owner as admins
            if let Some(pl) = body.pointer_mut("/initial_state/1/content/users")
                && let Some(obj) = pl.as_object_mut()
            {
                obj.insert(self.sender_user_id(), 100.into());
                obj.insert(self.owner_id.clone(), 100.into());
            }

            let resp = self
                .request(reqwest::Method::POST, "_matrix/client/v3/createRoom", None)
                .json(&body)
                .send()
                .await
                .map_err(|e| anyhow!("createRoom request failed: {}", e))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(
                    "createRoom failed ({}): {}. Trying alias recovery.",
                    status, body
                );

                // Recovery: another attempt may have won the race, or the room
                // already exists after a DB reset.
                let alias = format!(
                    "#cumments_{}_{}:{}",
                    site_id.as_str(),
                    post_slug.as_str(),
                    self.server_name
                );
                return match self.resolve_room_by_alias(&alias).await? {
                    Some(room_id) => {
                        info!(
                            "Recovered comment room {} after createRoom failure",
                            room_id
                        );
                        let _ = self
                            .set_room_metadata(&room_id, site_id, Some(post_slug))
                            .await;
                        self.link_room_to_space(space_id, &room_id).await;
                        Ok(room_id)
                    }
                    None => Err(anyhow!(
                        "Failed to create comment room ({}): {}",
                        status,
                        body
                    )),
                };
            }

            let data: CreateRoomResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("Failed to parse createRoom response: {}", e))?;
            data.room_id
        };

        // Keep the room linked to its Space (idempotent, best-effort).
        self.link_room_to_space(space_id, &room_id).await;

        Ok(room_id)
    }

    #[instrument(skip(self))]
    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        nickname: &str,
        // Public key and signature are published in the event so ownership
        // stays verifiable from Matrix alone.
        author_public_key: &str,
        author_signature: &str,
        site_id: &SiteId,
        intent_id: Option<i64>,
    ) -> Result<String> {
        // 1. Resolve virtual user via the store (includes site_id)
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        let visitor_id = derive_visitor_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow!("invalid author public key"))?;

        // 2. Ensure the virtual user is in the room (best-effort)
        self.ensure_joined(room_id, &virtual_user).await?;

        // 3. Send the message as the virtual user
        let formatted_body = format!("<strong>{}</strong>: {}", nickname, content);
        let message_body = serde_json::json!({
            "msgtype": "m.text",
            "body": format!("**{}**: {}", nickname, content),
            "format": "org.matrix.custom.html",
            "formatted_body": formatted_body,
            "cumments_visitor_id": visitor_id,
            "cumments_public_key": author_public_key,
            "cumments_signature": author_signature,
            // Structured fields so the projector can store the pure content
            // and nickname instead of parsing them back out of the body.
            "cumments_content": content,
            "cumments_nickname": nickname,
            "cumments_intent_id": intent_id,
        });

        let txn_id = self.txn_id();
        let path = format!(
            "_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            urlencode(room_id),
            txn_id
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, Some(&virtual_user))
            .json(&message_body)
            .send()
            .await
            .map_err(|e| anyhow!("postMessage request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to post message ({}): {}", status, body));
        }

        let data: SendEventResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse send response: {}", e))?;
        Ok(data.event_id)
    }

    #[instrument(skip(self))]
    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        nickname: &str,
        author_public_key: &str,
        author_signature: &str,
        site_id: &SiteId,
    ) -> Result<String> {
        // 1. Resolve virtual user (includes site_id)
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        let visitor_id = derive_visitor_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow!("invalid author public key"))?;

        // 2. Ensure joined (best-effort)
        self.ensure_joined(room_id, &virtual_user).await?;

        // 3. Send m.replace as the virtual user
        let formatted_content = format!("**{}**: {}", nickname, new_content);
        let message_body = serde_json::json!({
            "msgtype": "m.text",
            "body": format!(" * {}", formatted_content),
            "m.new_content": {
                "msgtype": "m.text",
                "body": formatted_content,
                "format": "org.matrix.custom.html",
                "formatted_body": format!("<strong>{}</strong>: {}", nickname, new_content),
                "cumments_visitor_id": visitor_id,
                "cumments_public_key": author_public_key,
                "cumments_signature": author_signature,
                "cumments_content": new_content,
                "cumments_nickname": nickname,
            },
            "m.relates_to": {
                "rel_type": "m.replace",
                "event_id": event_id,
            },
        });

        let txn_id = self.txn_id();
        let path = format!(
            "_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            urlencode(room_id),
            txn_id
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, Some(&virtual_user))
            .json(&message_body)
            .send()
            .await
            .map_err(|e| anyhow!("updateMessage request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to update message ({}): {}", status, body));
        }

        let data: SendEventResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse send response: {}", e))?;
        Ok(data.event_id)
    }

    #[instrument(skip(self))]
    async fn redact_message(&self, room_id: &str, event_id: &str) -> Result<()> {
        // Redact as the sender user (has admin power level in the room).
        let txn_id = self.txn_id();
        let path = format!(
            "_matrix/client/v3/rooms/{}/redact/{}/{}",
            urlencode(room_id),
            urlencode(event_id),
            txn_id
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, None)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| anyhow!("redactMessage request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("Failed to redact message ({}): {}", status, body);
            return Err(anyhow!("Failed to redact message ({}): {}", status, body));
        }

        Ok(())
    }

    #[instrument(skip(self))]
    async fn event_exists(&self, room_id: &str, event_id: &str) -> Result<bool> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/event/{}",
            urlencode(room_id),
            urlencode(event_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to query event {}: {}", event_id, e))?;

        if resp.status().is_success() {
            Ok(true)
        } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(false)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Event lookup {} failed ({}): {}",
                event_id,
                status,
                body
            ))
        }
    }

    #[instrument(skip(self))]
    async fn get_room_events(
        &self,
        room_id: &str,
        from: Option<&str>,
        limit: u32,
    ) -> Result<RoomEventPage> {
        let mut path = format!(
            "_matrix/client/v3/rooms/{}/messages?dir=b&limit={}",
            urlencode(room_id),
            limit
        );
        if let Some(from) = from {
            path.push_str(&format!("&from={}", urlencode(from)));
        }

        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to fetch room history: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to fetch room history for {} ({}): {}",
                room_id,
                status,
                body
            ));
        }

        let data: MessagesResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse messages response: {}", e))?;
        Ok(RoomEventPage {
            events: data.chunk,
            next_batch: Some(data.end.clone()),
            done: data.start == data.end,
        })
    }

    #[instrument(skip(self))]
    async fn joined_rooms(&self) -> Result<Vec<String>> {
        let resp = self
            .request(reqwest::Method::GET, "_matrix/client/v3/joined_rooms", None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to list joined rooms: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to list joined rooms ({}): {}",
                status,
                body
            ));
        }

        let data: JoinedRoomsResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse joined_rooms response: {}", e))?;
        Ok(data.joined_rooms)
    }

    async fn room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        self.get_room_metadata(room_id).await
    }
}

/// Percent-encode a string for safe use in URL path segments.
/// Matrix room IDs contain `!` and `:` — these are technically safe in
/// URL paths, but we encode them for correctness.
fn urlencode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_matches_space() {
        let meta = json!({"site_id": "my-blog", "post_slug": null});
        assert!(metadata_matches(&meta, "my-blog", None));
        assert!(!metadata_matches(&meta, "other", None));
        assert!(!metadata_matches(&meta, "my-blog", Some("hello")));
    }

    #[test]
    fn metadata_matches_space_without_slug_key() {
        let meta = json!({"site_id": "my-blog"});
        assert!(metadata_matches(&meta, "my-blog", None));
    }

    #[test]
    fn metadata_matches_comment_room() {
        let meta = json!({"site_id": "my-blog", "post_slug": "hello-world"});
        assert!(metadata_matches(&meta, "my-blog", Some("hello-world")));
        assert!(!metadata_matches(&meta, "my-blog", None));
        assert!(!metadata_matches(&meta, "my-blog", Some("other")));
    }

    #[test]
    fn urlencode_encodes_alias_hash_and_colon() {
        assert_eq!(
            urlencode("#cumments_my-blog:example.com"),
            "%23cumments_my-blog%3Aexample.com"
        );
        assert_eq!(urlencode("!abc:example.com"), "%21abc%3Aexample.com");
    }
}

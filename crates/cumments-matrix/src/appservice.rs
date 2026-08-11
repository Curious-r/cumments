//! AppService MatrixDriver – uses `reqwest` to call the Matrix CS API
//! directly with the AppService `as_token`, supporting virtual users.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::{
    identity::derive_guest_id_from_public_key,
    models::{PostSlug, RoomEventPage, SiteId},
    ports::{MatrixDriver, VirtualUserStore},
    protocol::{MESSAGE_CONTENT_KEY, ROOM_METADATA_EVENT_TYPE},
};
use rand::Rng;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
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

#[derive(Deserialize)]
struct CapabilitiesResponse {
    room_versions: Option<RoomVersions>,
}

#[derive(Deserialize)]
struct RoomVersions {
    default: String,
}

/// Preferred alias localpart prefix. The Matrix spec recommends exclusive
/// user and alias namespaces begin with `_` after the sigil, e.g.
/// `@_irc_.*` / `#_irc_.*`.
const ALIAS_PREFIX: &str = "_cumments_";
/// Upper bound for the joined-room cache. Membership changes are rare; when
/// the cap is hit the cache is reset and rebuilt from homeserver state.
const JOINED_CACHE_MAX: usize = 10_000;

fn site_space_alias_localpart(site_id: &str) -> String {
    format!("{}{}", ALIAS_PREFIX, site_id)
}

fn comment_room_alias_localpart(site_id: &str, post_slug: &str) -> String {
    format!("{}{}_{}", ALIAS_PREFIX, site_id, post_slug)
}

fn site_space_alias(server_name: &str, site_id: &str) -> String {
    format!("#{}:{}", site_space_alias_localpart(site_id), server_name)
}

fn comment_room_alias(server_name: &str, site_id: &str, post_slug: &str) -> String {
    format!(
        "#{}:{}",
        comment_room_alias_localpart(site_id, post_slug),
        server_name
    )
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

/// Whether a room version still requires the room creator to be listed in
/// `m.room.power_levels.users`.
///
/// Room versions 1-11 explicitly privilege the creator through the power
/// levels event (and allow that power to be changed). From version 12 onward
/// the creator is never listed in the event and instead holds immutable
/// infinite power. Unknown/absent versions are treated conservatively as
/// explicit (the pre-v12 behaviour).
fn room_requires_explicit_creator(version: Option<&str>) -> bool {
    let major = version
        .and_then(|v| v.split(['.', '-']).next())
        .and_then(|v| v.parse::<u32>().ok());
    major.is_none_or(|v| v < 12)
}

/// Build the `m.room.power_levels` content used when creating a Cumments room.
/// Omitted fields take the room-version defaults; the owner always receives
/// admin power, and the AS sender is only listed when the room version needs
/// an explicit creator entry.
fn initial_power_levels(
    sender_user_id: &str,
    owner_id: &str,
    include_sender: bool,
) -> serde_json::Value {
    let mut users = serde_json::Map::new();
    if include_sender {
        users.insert(sender_user_id.to_string(), 100.into());
    }
    users.insert(owner_id.to_string(), 100.into());
    serde_json::json!({ "users": users })
}

/// Returns a copy of a room's `m.room.power_levels` content with the owner
/// raised to admin (100), preserving every other field. Returns `None` when
/// the owner already has at least admin power.
fn power_levels_with_owner(
    content: &serde_json::Value,
    owner_id: &str,
) -> Option<serde_json::Value> {
    let level = content
        .get("users")
        .and_then(|u| u.get(owner_id))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if level >= 100 {
        return None;
    }

    let mut updated = content.clone();
    match updated
        .as_object_mut()
        .and_then(|obj| obj.get_mut("users"))
        .and_then(|u| u.as_object_mut())
    {
        Some(users) => {
            users.insert(owner_id.to_string(), 100.into());
        }
        None => {
            let obj = updated.as_object_mut()?;
            let mut users = serde_json::Map::new();
            users.insert(owner_id.to_string(), 100.into());
            obj.insert("users".to_string(), serde_json::Value::Object(users));
        }
    }
    Some(updated)
}

/// Whether `sender_user_id` can send state events in a room, according to its
/// `m.room.power_levels` content: the sender's explicit `users` entry (or
/// `users_default`) against the event-specific requirement for our metadata
/// event type (or `state_default`).
fn has_state_power(power_levels: &serde_json::Value, sender_user_id: &str) -> bool {
    let user_power = power_levels
        .get("users")
        .and_then(|u| u.get(sender_user_id))
        .and_then(|v| v.as_i64())
        .or_else(|| power_levels.get("users_default").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    let required = power_levels
        .get("events")
        .and_then(|e| e.get(ROOM_METADATA_EVENT_TYPE))
        .and_then(|v| v.as_i64())
        .or_else(|| power_levels.get("state_default").and_then(|v| v.as_i64()))
        .unwrap_or(50);
    user_power >= required
}

/// Build the rich-reply fallback body following the pre-v1.13 spec format:
/// each line of the original body is prefixed with `> `, the first line also
/// carries the original sender's MXID, followed by a blank line and the reply
/// content. Returns `None` when there is nothing useful to quote.
fn reply_fallback_body(sender_mxid: &str, original: &str, content: &str) -> Option<String> {
    if original.is_empty() {
        return None;
    }
    let mut quoted = original
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("> <{}> {}", sender_mxid, line)
            } else {
                format!("> {}", line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    quoted.push_str("\n\n");
    quoted.push_str(content);
    Some(quoted)
}

/// Build the `m.room.message` content for a new Cumments comment.
#[allow(clippy::too_many_arguments)] // wire-format builders carry the full event payload
fn build_message_body(
    content: &str,
    displayname: &str,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    guest_id: &str,
    intent_id: Option<i64>,
    reply_to: Option<&str>,
    reply_to_body: Option<&str>,
    reply_to_sender: Option<&str>,
) -> serde_json::Value {
    // The body is the pure comment text unless a rich-reply fallback is
    // available: the quoted original is only for clients without relation
    // support. The structured block always carries the pure content.
    let body = match (reply_to, reply_to_body, reply_to_sender) {
        (Some(_), Some(original), Some(sender)) => {
            reply_fallback_body(sender, original, content).unwrap_or_else(|| content.to_string())
        }
        _ => content.to_string(),
    };
    let mut message_body = serde_json::json!({
        "msgtype": "m.text",
        "body": body,
    });
    message_body[MESSAGE_CONTENT_KEY] = serde_json::json!({
        "guest_id": guest_id,
        "public_key": author_public_key,
        "signature": author_signature,
        "challenge": author_challenge,
        // Structured fields so the projector can store the pure content
        // and displayname instead of parsing them back out of the body.
        "content": content,
        "displayname": displayname,
        "intent_id": intent_id,
    });
    if let Some(parent_event_id) = reply_to {
        message_body["m.relates_to"] = serde_json::json!({
            "m.in_reply_to": {
                "event_id": parent_event_id,
            }
        });
    }
    message_body
}

/// Build the `m.room.message` content for an edit (`m.replace`).
#[allow(clippy::too_many_arguments)] // wire-format builders carry the full event payload
fn build_edit_body(
    event_id: &str,
    new_content: &str,
    displayname: &str,
    author_public_key: &str,
    author_signature: &str,
    author_challenge: &str,
    guest_id: &str,
    intent_id: Option<i64>,
) -> serde_json::Value {
    let mut new_content_obj = serde_json::json!({
        "msgtype": "m.text",
        "body": new_content,
    });
    new_content_obj[MESSAGE_CONTENT_KEY] = serde_json::json!({
        "guest_id": guest_id,
        "public_key": author_public_key,
        "signature": author_signature,
        "challenge": author_challenge,
        "content": new_content,
        "displayname": displayname,
        "intent_id": intent_id,
    });
    serde_json::json!({
        "msgtype": "m.text",
        // Matrix edits require a fallback body starting with "* ".
        "body": format!("* {}", new_content),
        "m.new_content": new_content_obj,
        "m.relates_to": {
            "rel_type": "m.replace",
            "event_id": event_id,
        },
    })
}

/// The AppService-based Matrix driver.
///
/// This driver authenticates with the AppService `as_token` and can
/// impersonate any virtual user in the AppService namespace.
pub struct AppServiceMatrixDriver {
    http_client: reqwest::Client,
    homeserver_url: String,
    as_token: String,
    server_name: String,
    sender_localpart: String,
    owner_id: String,
    virtual_user_store: Arc<dyn VirtualUserStore>,
    joined_cache: Mutex<HashSet<(String, String)>>,
    displayname_cache: Mutex<HashMap<String, String>>,
    /// Explicit room version from configuration, if any.
    room_version_override: Option<String>,
    /// Cached `m.room_versions.default` from `/capabilities`.
    default_room_version: Mutex<Option<String>>,
}

impl AppServiceMatrixDriver {
    pub fn new(
        homeserver_url: String,
        as_token: String,
        server_name: String,
        sender_localpart: String,
        owner_id: String,
        virtual_user_store: Arc<dyn VirtualUserStore>,
        room_version: Option<String>,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        Self {
            http_client,
            homeserver_url,
            as_token,
            server_name,
            sender_localpart,
            owner_id,
            virtual_user_store,
            joined_cache: Mutex::new(HashSet::new()),
            displayname_cache: Mutex::new(HashMap::new()),
            room_version_override: room_version,
            default_room_version: Mutex::new(None),
        }
    }

    // ── Internal helpers ──────────────────────────────────────────

    /// Build a fully qualified user ID from a localpart.
    fn user_id(&self, localpart: &str) -> String {
        format!("@{}:{}", localpart, self.server_name)
    }

    /// The AppService sender user ID used for room creation, state events
    /// and redactions.
    fn sender_user_id(&self) -> String {
        self.user_id(&self.sender_localpart)
    }

    /// Generate a transaction ID for idempotent requests.
    ///
    /// When an intent ID is available the txn ID is deterministic: if the
    /// homeserver accepted the first attempt but the response was lost, a
    /// retry with the same txn ID returns the original event instead of
    /// creating a duplicate.
    ///
    /// The `kind` is part of the ID because homeservers scope transaction-ID
    /// deduplication per (user, device, txn_id) without considering the
    /// endpoint. Post and update intents are both sent by the same virtual
    /// user through `/send`, so separate queues with colliding ids would
    /// otherwise make an edit replay the original post.
    fn txn_id(&self, kind: &str, intent_id: Option<i64>) -> String {
        format_txn_id(kind, intent_id)
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
        let cache_key = (room_id.to_owned(), virtual_user.to_owned());
        if self.is_joined_cached(&cache_key) {
            return Ok(());
        }

        // Check membership from the AS sender's perspective (it created the
        // room and holds state access) so the result is authoritative for
        // users that are already joined.
        if self.is_joined(room_id, virtual_user).await {
            self.cache_joined(cache_key);
            return Ok(());
        }

        let path = format!("_matrix/client/v3/rooms/{}/join", urlencode(room_id));
        let resp = self
            .request(reqwest::Method::POST, &path, Some(virtual_user))
            .send()
            .await
            .map_err(|e| anyhow!("Join request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            let errcode = serde_json::from_str::<serde_json::Value>(&error_body)
                .ok()
                .and_then(|v| v.get("errcode").and_then(|e| e.as_str()).map(str::to_owned))
                .unwrap_or_default();
            if errcode == "M_FORBIDDEN" {
                // M_FORBIDDEN can mean "already joined", banned, or denied by
                // join rules; do not guess. Re-check membership from the AS
                // sender's perspective and only treat a confirmed join as
                // success. Anything else is left for the send to resolve.
                if self.is_joined(room_id, virtual_user).await {
                    self.cache_joined(cache_key);
                    return Ok(());
                }
                self.invalidate_joined(&cache_key);
                warn!(
                    "Virtual user {} join room {} returned M_FORBIDDEN and is not joined ({}): {}",
                    virtual_user, room_id, status, error_body
                );
            } else {
                warn!(
                    "Virtual user {} join room {} failed ({}): {}",
                    virtual_user, room_id, status, error_body
                );
            }
        } else {
            self.cache_joined(cache_key);
        }
        Ok(())
    }

    /// Whether the virtual user has `join` membership in the room, queried as
    /// the AS sender. Errors are logged and treated as "not joined" so the
    /// subsequent `/join` (or the final send) remains the authority.
    async fn is_joined(&self, room_id: &str, virtual_user: &str) -> bool {
        let member_path = format!(
            "_matrix/client/v3/rooms/{}/state/m.room.member/{}",
            urlencode(room_id),
            urlencode(virtual_user)
        );
        let resp = match self
            .request(reqwest::Method::GET, &member_path, None)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                warn!(
                    "Membership check for {} in {} failed: {:?}",
                    virtual_user, room_id, e
                );
                return false;
            }
        };

        if resp.status().is_success() {
            match resp.json::<serde_json::Value>().await {
                Ok(content) => content.get("membership").and_then(|v| v.as_str()) == Some("join"),
                Err(e) => {
                    warn!(
                        "Failed to parse membership for {} in {}: {:?}",
                        virtual_user, room_id, e
                    );
                    false
                }
            }
        } else {
            if resp.status() != reqwest::StatusCode::NOT_FOUND {
                warn!(
                    "Membership check for {} in {} failed ({}): {}",
                    virtual_user,
                    room_id,
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
            false
        }
    }

    fn is_joined_cached(&self, cache_key: &(String, String)) -> bool {
        self.joined_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(cache_key)
    }

    fn cache_joined(&self, cache_key: (String, String)) {
        let mut cache = self.joined_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= JOINED_CACHE_MAX {
            cache.clear();
        }
        cache.insert(cache_key);
    }

    fn invalidate_joined(&self, cache_key: &(String, String)) {
        self.joined_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(cache_key);
    }

    /// Best-effort: keep the virtual user's display name in sync with the
    /// commenter's display name so Matrix clients show it instead of
    /// the localpart. Failures only warn; the message send still proceeds.
    async fn ensure_displayname(&self, virtual_user: &str, displayname: &str) -> Result<()> {
        {
            let cache = self
                .displayname_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cache.get(virtual_user).map(String::as_str) == Some(displayname) {
                return Ok(());
            }
        }

        let path = format!(
            "_matrix/client/v3/profile/{}/displayname",
            urlencode(virtual_user)
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, Some(virtual_user))
            .json(&serde_json::json!({ "displayname": displayname }))
            .send()
            .await
            .map_err(|e| anyhow!("set displayname request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "set displayname for {} failed ({}): {}",
                virtual_user,
                status,
                error_body
            ));
        }
        self.displayname_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(virtual_user.to_owned(), displayname.to_owned());
        Ok(())
    }

    /// Set Cumments metadata on a room (state event
    /// `host.curious.cumments.metadata`). Returns an error when the
    /// homeserver rejected the write.
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
            "_matrix/client/v3/rooms/{}/state/{}",
            urlencode(room_id),
            ROOM_METADATA_EVENT_TYPE
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, None)
            .json(&content)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to set room metadata: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Setting metadata for room {} failed ({}): {}",
                room_id,
                status,
                error_body
            ));
        }
        Ok(())
    }

    /// Query a room's metadata state event.
    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/{}",
            urlencode(room_id),
            ROOM_METADATA_EVENT_TYPE
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to query room metadata: {}", e))?;

        if resp.status().is_success() {
            Ok(Some(resp.json().await?))
        } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(None)
        } else {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Failed to query room metadata ({}): {}",
                status,
                error_body
            ))
        }
    }

    /// Whether the room was created as a Matrix Space (`m.room.create` with
    /// `type: "m.space"`).
    async fn room_is_space(&self, room_id: &str) -> Result<bool> {
        Ok(match self.get_room_create(room_id).await? {
            Some(create) => {
                create
                    .get("content")
                    .and_then(|c| c.get("type"))
                    .and_then(|v| v.as_str())
                    == Some("m.space")
            }
            None => false,
        })
    }

    /// Fetch the room's `m.room.create` event, if the room exists.
    async fn get_room_create(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/m.room.create",
            urlencode(room_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to query room create state: {}", e))?;

        if resp.status().is_success() {
            Ok(Some(resp.json().await?))
        } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(None)
        } else {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Failed to query room create state ({}): {}",
                status,
                error_body
            ))
        }
    }

    /// The room version used for newly created rooms: the explicit
    /// configuration value if set, otherwise the homeserver's default from
    /// `/capabilities` (cached after the first lookup). `None` means unknown;
    /// callers then assume the pre-v12 behaviour.
    async fn effective_room_version(&self) -> Option<String> {
        if let Some(version) = &self.room_version_override {
            return Some(version.clone());
        }
        if let Some(version) = self
            .default_room_version
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Some(version);
        }

        let resp = match self
            .request(reqwest::Method::GET, "_matrix/client/v3/capabilities", None)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                warn!("Failed to query homeserver capabilities: {:#}", e);
                return None;
            }
        };
        if !resp.status().is_success() {
            warn!(
                "Failed to query homeserver capabilities ({}): {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
            return None;
        }

        let version = match resp.json::<CapabilitiesResponse>().await {
            Ok(caps) => caps
                .room_versions
                .and_then(|r| (!r.default.is_empty()).then_some(r.default)),
            Err(e) => {
                warn!("Failed to parse homeserver capabilities: {:#}", e);
                return None;
            }
        };
        if let Some(version) = &version {
            *self
                .default_room_version
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(version.clone());
        }
        version
    }

    /// Read a room's current `m.room.power_levels` content, if any.
    async fn get_power_levels(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/m.room.power_levels",
            urlencode(room_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to query power levels: {}", e))?;

        if resp.status().is_success() {
            Ok(Some(resp.json().await?))
        } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(None)
        } else {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Failed to query power levels ({}): {}",
                status,
                error_body
            ))
        }
    }

    /// Write a room's `m.room.power_levels` content (full state replacement).
    async fn write_power_levels(&self, room_id: &str, content: &serde_json::Value) -> Result<()> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/m.room.power_levels",
            urlencode(room_id)
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, None)
            .json(content)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to set power levels: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to set power levels ({}): {}",
                status,
                error_body
            ));
        }
        Ok(())
    }

    /// Whether the AS sender can write state events in the room. Reading the
    /// power levels doubles as a membership check: non-members get an error.
    async fn sender_can_write_state(&self, room_id: &str) -> Result<bool> {
        // Room version 12+ gives the creator immutable infinite power without
        // listing them in `m.room.power_levels.users`; account for that before
        // falling back to the power-levels check.
        if let Some(create) = self.get_room_create(room_id).await? {
            let creator = create.get("sender").and_then(|v| v.as_str());
            let version = create
                .get("content")
                .and_then(|c| c.get("room_version"))
                .and_then(|v| v.as_str());
            if !room_requires_explicit_creator(version)
                && creator == Some(self.sender_user_id().as_str())
            {
                return Ok(true);
            }
        }
        Ok(match self.get_power_levels(room_id).await? {
            Some(power_levels) => has_state_power(&power_levels, &self.sender_user_id()),
            // No power-levels event: Matrix defaults apply and leave the
            // sender below the state-writing threshold, so treat as unable.
            None => false,
        })
    }

    /// Admission guard before adopting a room found via our exclusive alias
    /// namespace.
    ///
    /// The trust anchor is the homeserver-enforced exclusive `#_cumments_*`
    /// namespace: a room under one of our canonical aliases should be one we
    /// created. As a belt-and-suspenders check (e.g. if the registration
    /// namespace is misconfigured, or the room was created by an old run with
    /// a different sender), refuse to take over a room the AS sender cannot
    /// actually govern: it must be a member and able to write state events.
    /// A room we cannot govern would let a third party steer comments into an
    /// uncontrolled room, so we fail loudly instead of silently adopting it.
    async fn ensure_room_adoptable(&self, room_id: &str) -> Result<()> {
        match self.sender_can_write_state(room_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(anyhow!(
                "Refusing to adopt room {}: AS sender cannot write state \
                 (not a member or insufficient power)",
                room_id
            )),
            Err(e) => Err(anyhow!(
                "Cannot verify governance of room {} before adoption: {:#}",
                room_id,
                e
            )),
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
            let error_body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Alias lookup {} failed ({}): {}",
                alias,
                status,
                error_body
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
        let child_resp = self
            .request(reqwest::Method::PUT, &space_path, None)
            .json(&child_content)
            .send()
            .await;
        match child_resp {
            Err(e) => {
                warn!(
                    "Failed to link room {} to space {}: {}",
                    room_id, space_id, e
                );
            }
            Ok(resp) if !resp.status().is_success() => {
                let status = resp.status();
                let error_body = resp.text().await.unwrap_or_default();
                warn!(
                    "Failed to link room {} to space {} ({}): {}",
                    room_id, space_id, status, error_body
                );
            }
            _ => {}
        }

        let parent_path = format!(
            "_matrix/client/v3/rooms/{}/state/m.space.parent/{}",
            urlencode(room_id),
            urlencode(space_id)
        );
        let parent_content = serde_json::json!({
            "via": [self.server_name],
            "canonical": true
        });
        let parent_resp = self
            .request(reqwest::Method::PUT, &parent_path, None)
            .json(&parent_content)
            .send()
            .await;
        match parent_resp {
            Err(e) => {
                warn!(
                    "Failed to link space {} as parent of room {}: {}",
                    space_id, room_id, e
                );
            }
            Ok(resp) if !resp.status().is_success() => {
                let status = resp.status();
                let error_body = resp.text().await.unwrap_or_default();
                warn!(
                    "Failed to link space {} as parent of room {} ({}): {}",
                    space_id, room_id, status, error_body
                );
            }
            _ => {}
        }
    }
}

#[async_trait]
impl MatrixDriver for AppServiceMatrixDriver {
    #[instrument(skip(self), fields(site_id = %site_id.as_str()))]
    async fn create_site_space(&self, site_id: &SiteId) -> Result<String> {
        let site_id_str = site_id.as_str();
        let alias_localpart = site_space_alias_localpart(site_id_str);

        info!("Creating new space for site {} via AppService", site_id_str);

        let include_sender =
            room_requires_explicit_creator(self.effective_room_version().await.as_deref());
        let power_levels =
            initial_power_levels(&self.sender_user_id(), &self.owner_id, include_sender);

        let mut body = serde_json::json!({
            "name": format!("Comments: {}", site_id_str),
            "room_alias_name": alias_localpart,
            "creation_content": {
                "type": "m.space"
            },
            "initial_state": [
                {
                    "type": ROOM_METADATA_EVENT_TYPE,
                    "state_key": "",
                    "content": {
                        "site_id": site_id_str,
                        "post_slug": null
                    }
                },
                {
                    "type": "m.room.power_levels",
                    "state_key": "",
                    "content": power_levels
                }
            ],
            "invite": [self.owner_id.clone()],
            "preset": "public_chat",
        });

        if let Some(version) = &self.room_version_override {
            body["room_version"] = serde_json::json!(version);
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
            let error_body = resp.text().await.unwrap_or_default();
            warn!(
                "createRoom failed ({}): {}. Trying alias recovery.",
                status, error_body
            );

            // Recovery: after a local DB reset (or a partial previous run) the
            // space may already exist under our exclusive alias. Adopt it.
            let alias = site_space_alias(&self.server_name, site_id_str);
            match self.resolve_room_by_alias(&alias).await? {
                Some(room_id) => {
                    if !self.room_is_space(&room_id).await? {
                        warn!(
                            "Refusing to adopt room {} as site space: not created as m.space",
                            room_id
                        );
                        return Err(anyhow!(
                            "Alias {} resolved to a non-space room; not adopting",
                            alias
                        ));
                    }
                    if self.room_metadata_matches(&room_id, site_id, None).await? {
                        info!(
                            "Recovered existing site space {} via alias {}",
                            room_id, alias
                        );
                    } else {
                        self.ensure_room_adoptable(&room_id).await?;
                        warn!(
                            "Adopting room {} under alias {} with repaired metadata",
                            room_id, alias
                        );
                        if let Err(e) = self.set_room_metadata(&room_id, site_id, None).await {
                            warn!(
                                "Failed to repair space metadata for room {}: {:#}",
                                room_id, e
                            );
                        }
                    }
                    self.ensure_owner_admin(&room_id).await;
                    Ok(room_id)
                }
                None => Err(anyhow!(
                    "Failed to create site space ({}): {}",
                    status,
                    error_body
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
            let alias = comment_room_alias(&self.server_name, site_id.as_str(), post_slug.as_str());
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
                    self.ensure_room_adoptable(&room_id).await?;
                    warn!(
                        "Adopting room {} under alias {} with repaired metadata",
                        room_id, alias
                    );
                    if let Err(e) = self
                        .set_room_metadata(&room_id, site_id, Some(post_slug))
                        .await
                    {
                        warn!(
                            "Failed to repair comment room metadata for {}: {:#}",
                            room_id, e
                        );
                    }
                }
                self.ensure_owner_admin(&room_id).await;
                target_room_id = Some(room_id);
            }
        }

        let room_id = if let Some(id) = target_room_id {
            id
        } else {
            // ── PHASE 1: Create new comment room ──
            info!("No matching room found. Creating new comment room via AppService.");
            let alias_localpart =
                comment_room_alias_localpart(site_id.as_str(), post_slug.as_str());

            let include_sender =
                room_requires_explicit_creator(self.effective_room_version().await.as_deref());
            let power_levels =
                initial_power_levels(&self.sender_user_id(), &self.owner_id, include_sender);

            let mut body = serde_json::json!({
                "name": format!("Comments: {}/{}", site_id.as_str(), post_slug.as_str()),
                "room_alias_name": alias_localpart,
                "initial_state": [
                    {
                        "type": ROOM_METADATA_EVENT_TYPE,
                        "state_key": "",
                        "content": {
                            "site_id": site_id.as_str(),
                            "post_slug": post_slug.as_str()
                        }
                    },
                    {
                        "type": "m.room.power_levels",
                        "state_key": "",
                        "content": power_levels
                    }
                ],
                "invite": [self.owner_id.clone()],
                "preset": "public_chat",
            });

            if let Some(version) = &self.room_version_override {
                body["room_version"] = serde_json::json!(version);
            }

            let resp = self
                .request(reqwest::Method::POST, "_matrix/client/v3/createRoom", None)
                .json(&body)
                .send()
                .await
                .map_err(|e| anyhow!("createRoom request failed: {}", e))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let error_body = resp.text().await.unwrap_or_default();
                warn!(
                    "createRoom failed ({}): {}. Trying alias recovery.",
                    status, error_body
                );

                // Recovery: another attempt may have won the race, or the room
                // already exists after a DB reset.
                let alias =
                    comment_room_alias(&self.server_name, site_id.as_str(), post_slug.as_str());
                return match self.resolve_room_by_alias(&alias).await? {
                    Some(room_id) => {
                        if self
                            .room_metadata_matches(&room_id, site_id, Some(post_slug))
                            .await?
                        {
                            info!(
                                "Recovered comment room {} after createRoom failure via alias {}",
                                room_id, alias
                            );
                        } else {
                            self.ensure_room_adoptable(&room_id).await?;
                            warn!(
                                "Adopting room {} after createRoom failure via alias {} with repaired metadata",
                                room_id, alias
                            );
                            if let Err(e) = self
                                .set_room_metadata(&room_id, site_id, Some(post_slug))
                                .await
                            {
                                warn!(
                                    "Failed to repair comment room metadata for {}: {:#}",
                                    room_id, e
                                );
                            }
                        }
                        self.link_room_to_space(space_id, &room_id).await;
                        self.ensure_owner_admin(&room_id).await;
                        Ok(room_id)
                    }
                    None => Err(anyhow!(
                        "Failed to create comment room ({}): {}",
                        status,
                        error_body
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
        displayname: &str,
        // Public key and signature are published in the event so ownership
        // stays verifiable from Matrix alone.
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        reply_to: Option<&str>,
        reply_to_body: Option<&str>,
        reply_to_sender: Option<&str>,
        intent_id: Option<i64>,
    ) -> Result<String> {
        // 1. Resolve virtual user via the store (includes site_id)
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        let guest_id = derive_guest_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow!("invalid author public key"))?;

        // 2. Ensure the virtual user is in the room (best-effort)
        self.ensure_joined(room_id, &virtual_user).await?;

        // 2b. Keep the display name in sync (best-effort)
        if let Err(e) = self.ensure_displayname(&virtual_user, displayname).await {
            warn!("Failed to set display name for {}: {:#}", virtual_user, e);
        }

        // 3. Send the message as the virtual user
        let message_body = build_message_body(
            content,
            displayname,
            author_public_key,
            author_signature,
            author_challenge,
            &guest_id,
            intent_id,
            reply_to,
            reply_to_body,
            reply_to_sender,
        );

        let txn_id = self.txn_id("post", intent_id);
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
            let error_body = resp.text().await.unwrap_or_default();
            if error_body.contains("M_FORBIDDEN") || error_body.contains("M_NOT_FOUND") {
                self.invalidate_joined(&(room_id.to_owned(), virtual_user.clone()));
            }
            return Err(anyhow!(
                "Failed to post message ({}): {}",
                status,
                error_body
            ));
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
        displayname: &str,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        intent_id: Option<i64>,
    ) -> Result<String> {
        // 1. Resolve virtual user (includes site_id)
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        let guest_id = derive_guest_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow!("invalid author public key"))?;

        // 2. Ensure joined (best-effort)
        self.ensure_joined(room_id, &virtual_user).await?;

        // 2b. Keep the display name in sync (best-effort)
        if let Err(e) = self.ensure_displayname(&virtual_user, displayname).await {
            warn!("Failed to set display name for {}: {:#}", virtual_user, e);
        }

        // 3. Send m.replace as the virtual user
        let message_body = build_edit_body(
            event_id,
            new_content,
            displayname,
            author_public_key,
            author_signature,
            author_challenge,
            &guest_id,
            intent_id,
        );

        let txn_id = self.txn_id("update", intent_id);
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
            let error_body = resp.text().await.unwrap_or_default();
            if error_body.contains("M_FORBIDDEN") || error_body.contains("M_NOT_FOUND") {
                self.invalidate_joined(&(room_id.to_owned(), virtual_user.clone()));
            }
            return Err(anyhow!(
                "Failed to update message ({}): {}",
                status,
                error_body
            ));
        }

        let data: SendEventResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse send response: {}", e))?;
        Ok(data.event_id)
    }

    #[instrument(skip(self))]
    async fn redact_message(
        &self,
        room_id: &str,
        event_id: &str,
        intent_id: Option<i64>,
        proof: Option<&serde_json::Value>,
    ) -> Result<()> {
        // Redact as the sender user (has admin power level in the room).
        let txn_id = self.txn_id("delete", intent_id);
        let path = format!(
            "_matrix/client/v3/rooms/{}/redact/{}/{}",
            urlencode(room_id),
            urlencode(event_id),
            txn_id
        );
        let body = build_redaction_body(proof);
        let resp = self
            .request(reqwest::Method::PUT, &path, None)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("redactMessage request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            warn!("Failed to redact message ({}): {}", status, error_body);
            return Err(anyhow!(
                "Failed to redact message ({}): {}",
                status,
                error_body
            ));
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
            let error_body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Event lookup {} failed ({}): {}",
                event_id,
                status,
                error_body
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
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to fetch room history for {} ({}): {}",
                room_id,
                status,
                error_body
            ));
        }

        let data: MessagesResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse messages response: {}", e))?;
        Ok(RoomEventPage {
            events: data.chunk,
            next_token: Some(data.end.clone()),
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
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to list joined rooms ({}): {}",
                status,
                error_body
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

    #[instrument(skip(self))]
    async fn room_canonical_alias(&self, room_id: &str) -> Result<Option<String>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/m.room.canonical_alias",
            urlencode(room_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to query canonical alias: {}", e))?;

        if resp.status().is_success() {
            let content: serde_json::Value = resp.json().await?;
            Ok(content
                .get("alias")
                .and_then(|v| v.as_str())
                .map(String::from))
        } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(None)
        } else {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Failed to query canonical alias ({}): {}",
                status,
                error_body
            ))
        }
    }

    #[instrument(skip(self))]
    async fn ensure_owner_admin(&self, room_id: &str) {
        // Best-effort: failures are logged here and never fail the caller.
        let updated = match self.get_power_levels(room_id).await {
            Ok(Some(content)) => match power_levels_with_owner(&content, &self.owner_id) {
                Some(updated) => updated,
                None => return, // owner already has admin power
            },
            // No power-levels event: room defaults apply, so create one that
            // grants the owner admin.
            Ok(None) => serde_json::json!({ "users": { self.owner_id.clone(): 100 } }),
            Err(e) => {
                warn!("Failed to read power levels for room {}: {:#}", room_id, e);
                return;
            }
        };

        match self.write_power_levels(room_id, &updated).await {
            Ok(()) => info!("Granted owner admin power in room {}", room_id),
            Err(e) => warn!("Failed to grant owner admin in room {}: {:#}", room_id, e),
        }
    }
}

/// Build the `m.room.redaction` content. When a delete proof is available it
/// is embedded as a JSON string in `reason` so the event log carries the
/// authorization independently of the intent queue.
fn build_redaction_body(proof: Option<&serde_json::Value>) -> serde_json::Value {
    match proof {
        Some(proof) => serde_json::json!({ "reason": proof.to_string() }),
        None => serde_json::json!({}),
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

/// Deterministic transaction ID for intent-driven requests.
///
/// The operation kind is part of the ID so distinct intent queues can never
/// collide on a homeserver that deduplicates by `(user, device, txn_id)`.
fn format_txn_id(kind: &str, intent_id: Option<i64>) -> String {
    match intent_id {
        Some(id) => format!("cumments_{}_{}", kind, id),
        None => {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let mut random_bytes = [0u8; 4];
            rand::rng().fill_bytes(&mut random_bytes);
            format!("cumments_{}_{}_{}", kind, ts, hex::encode(random_bytes))
        }
    }
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
    fn power_levels_owner_already_admin_is_unchanged() {
        let content = json!({
            "ban": 50,
            "users": { "@owner:example.com": 100, "@other:example.com": 0 }
        });
        assert!(power_levels_with_owner(&content, "@owner:example.com").is_none());
    }

    #[test]
    fn power_levels_raises_owner_to_admin_and_preserves_rest() {
        let content = json!({
            "ban": 50,
            "state_default": 50,
            "users": { "@owner:example.com": 50, "@other:example.com": 0 }
        });
        let updated = power_levels_with_owner(&content, "@owner:example.com")
            .expect("owner should be raised to admin");
        assert_eq!(updated["users"]["@owner:example.com"], 100);
        assert_eq!(updated["users"]["@other:example.com"], 0);
        assert_eq!(updated["ban"], 50);
        assert_eq!(updated["state_default"], 50);
    }

    #[test]
    fn power_levels_adds_missing_owner_entry() {
        let content = json!({ "users": { "@other:example.com": 0 } });
        let updated =
            power_levels_with_owner(&content, "@owner:example.com").expect("owner should be added");
        assert_eq!(updated["users"]["@owner:example.com"], 100);
        assert_eq!(updated["users"]["@other:example.com"], 0);
    }

    #[test]
    fn power_levels_creates_users_map_when_missing() {
        let content = json!({ "ban": 50 });
        let updated = power_levels_with_owner(&content, "@owner:example.com")
            .expect("users map should be created");
        assert_eq!(updated["users"]["@owner:example.com"], 100);
        assert_eq!(updated["ban"], 50);
    }

    #[test]
    fn state_power_explicit_admin_can_write_state() {
        let pl = json!({
            "users": { "@_cumments_bot:example.com": 100 },
            "state_default": 50
        });
        assert!(has_state_power(&pl, "@_cumments_bot:example.com"));
    }

    #[test]
    fn state_power_default_member_cannot_write_state() {
        let pl = json!({ "state_default": 50 });
        assert!(!has_state_power(&pl, "@_cumments_bot:example.com"));
    }

    #[test]
    fn state_power_users_default_can_write_state() {
        let pl = json!({ "users_default": 50, "state_default": 50 });
        assert!(has_state_power(&pl, "@_cumments_bot:example.com"));
    }

    #[test]
    fn state_power_event_specific_override_is_respected() {
        let pl = json!({
            "users": { "@_cumments_bot:example.com": 50 },
            "state_default": 50,
            "events": { ROOM_METADATA_EVENT_TYPE: 100 }
        });
        assert!(!has_state_power(&pl, "@_cumments_bot:example.com"));
    }

    #[test]
    fn state_power_missing_fields_fall_back_to_defaults() {
        let pl = json!({ "users": { "@_cumments_bot:example.com": 49 } });
        assert!(!has_state_power(&pl, "@_cumments_bot:example.com"));
        let pl = json!({ "users": { "@_cumments_bot:example.com": 50 } });
        assert!(has_state_power(&pl, "@_cumments_bot:example.com"));
    }

    #[test]
    fn urlencode_encodes_alias_hash_and_colon() {
        assert_eq!(
            urlencode("#_cumments_my-blog:example.com"),
            "%23_cumments_my-blog%3Aexample.com"
        );
        assert_eq!(urlencode("!abc:example.com"), "%21abc%3Aexample.com");
    }

    #[test]
    fn alias_helpers_build_underscored_aliases() {
        assert_eq!(
            site_space_alias("example.com", "my-blog"),
            "#_cumments_my-blog:example.com"
        );
        assert_eq!(
            comment_room_alias("example.com", "my-blog", "hello-world"),
            "#_cumments_my-blog_hello-world:example.com"
        );
    }

    #[test]
    fn message_body_uses_namespaced_content_block() {
        let body = build_message_body(
            "hello <b>",
            "Alice",
            "pubkey",
            "sig",
            "chal",
            "abcd",
            Some(7),
            None,
            None,
            None,
        );

        assert_eq!(body["msgtype"].as_str(), Some("m.text"));
        assert_eq!(body["body"].as_str(), Some("hello <b>"));
        assert!(body.get("format").is_none());
        assert!(body.get("formatted_body").is_none());

        let ns = body.get(MESSAGE_CONTENT_KEY).expect("namespaced block");
        assert_eq!(ns["guest_id"].as_str(), Some("abcd"));
        assert_eq!(ns["public_key"].as_str(), Some("pubkey"));
        assert_eq!(ns["signature"].as_str(), Some("sig"));
        assert_eq!(ns["challenge"].as_str(), Some("chal"));
        assert_eq!(ns["content"].as_str(), Some("hello <b>"));
        assert_eq!(ns["displayname"].as_str(), Some("Alice"));
        assert_eq!(ns["intent_id"].as_i64(), Some(7));

        assert!(body.get("cumments_guest_id").is_none());
        assert!(body.get("cumments_intent_id").is_none());
    }

    #[test]
    fn message_body_with_reply_uses_standard_relation() {
        let body = build_message_body(
            "hello",
            "Alice",
            "pubkey",
            "sig",
            "chal",
            "abcd",
            Some(7),
            Some("$parent:hs"),
            Some("original line one\noriginal line two"),
            Some("@alice:hs"),
        );

        assert_eq!(
            body["body"].as_str(),
            Some("> <@alice:hs> original line one\n> original line two\n\nhello")
        );
        // The structured block still carries only the pure reply content.
        assert_eq!(body[MESSAGE_CONTENT_KEY]["content"].as_str(), Some("hello"));
        assert_eq!(
            body["m.relates_to"]["m.in_reply_to"]["event_id"].as_str(),
            Some("$parent:hs")
        );
        assert!(body["m.relates_to"].get("rel_type").is_none());
        assert!(body.get(MESSAGE_CONTENT_KEY).is_some());
    }

    #[test]
    fn reply_without_known_original_keeps_pure_body() {
        let body = build_message_body(
            "hello",
            "Alice",
            "pubkey",
            "sig",
            "chal",
            "abcd",
            Some(7),
            Some("$parent:hs"),
            None,
            None,
        );
        assert_eq!(body["body"].as_str(), Some("hello"));
        assert_eq!(
            body["m.relates_to"]["m.in_reply_to"]["event_id"].as_str(),
            Some("$parent:hs")
        );
    }

    #[test]
    fn reply_fallback_quotes_multiline_original() {
        assert_eq!(
            reply_fallback_body("@alice:hs", "first\nsecond", "reply").as_deref(),
            Some("> <@alice:hs> first\n> second\n\nreply")
        );
        assert_eq!(reply_fallback_body("@alice:hs", "", "reply"), None);
    }

    #[test]
    fn edit_body_uses_namespaced_block_in_new_content() {
        let body = build_edit_body(
            "$original:hs",
            "edited <b>",
            "Alice",
            "pubkey",
            "sig",
            "chal",
            "abcd",
            Some(42),
        );

        assert_eq!(body["msgtype"].as_str(), Some("m.text"));
        assert_eq!(body["body"].as_str(), Some("* edited <b>"));
        assert_eq!(body["m.relates_to"]["rel_type"].as_str(), Some("m.replace"));
        assert_eq!(
            body["m.relates_to"]["event_id"].as_str(),
            Some("$original:hs")
        );

        let new_content = body.get("m.new_content").expect("new content");
        assert_eq!(new_content["body"].as_str(), Some("edited <b>"));
        assert!(new_content.get("format").is_none());
        assert!(new_content.get("formatted_body").is_none());
        let ns = new_content
            .get(MESSAGE_CONTENT_KEY)
            .expect("namespaced block");
        assert_eq!(ns["guest_id"].as_str(), Some("abcd"));
        assert_eq!(ns["public_key"].as_str(), Some("pubkey"));
        assert_eq!(ns["signature"].as_str(), Some("sig"));
        assert_eq!(ns["challenge"].as_str(), Some("chal"));
        assert_eq!(ns["content"].as_str(), Some("edited <b>"));
        assert_eq!(ns["displayname"].as_str(), Some("Alice"));
        assert_eq!(ns["intent_id"].as_i64(), Some(42));

        assert!(body.get(MESSAGE_CONTENT_KEY).is_none());
        assert!(body.get("cumments_intent_id").is_none());
    }

    #[test]
    fn redaction_body_embeds_proof_as_reason() {
        let proof = json!({
            "host.curious.cumments.redaction": {
                "public_key": "pk",
                "signature": "sig",
                "challenge": "chal",
            }
        });
        let body = build_redaction_body(Some(&proof));
        assert_eq!(body["reason"], proof.to_string());
        assert_eq!(build_redaction_body(None), json!({}));
    }

    #[test]
    fn txn_ids_are_deterministic_and_namespaced_per_operation() {
        assert_eq!(format_txn_id("post", Some(7)), "cumments_post_7");
        assert_eq!(format_txn_id("update", Some(7)), "cumments_update_7");
        assert_eq!(format_txn_id("delete", Some(7)), "cumments_delete_7");
        // Same intent id in different queues must never produce the same txn id.
        let ids: std::collections::HashSet<_> = [
            format_txn_id("post", Some(7)),
            format_txn_id("update", Some(7)),
            format_txn_id("delete", Some(7)),
        ]
        .into_iter()
        .collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn txn_ids_without_intent_are_random_but_namespaced() {
        let a = format_txn_id("post", None);
        let b = format_txn_id("post", None);
        assert!(a.starts_with("cumments_post_"));
        assert!(b.starts_with("cumments_post_"));
        assert_ne!(a, b);
    }

    #[test]
    fn explicit_creator_required_only_below_room_version_12() {
        assert!(room_requires_explicit_creator(None));
        assert!(room_requires_explicit_creator(Some("1")));
        assert!(room_requires_explicit_creator(Some("11")));
        assert!(room_requires_explicit_creator(Some("1.2")));
        assert!(!room_requires_explicit_creator(Some("12")));
        assert!(!room_requires_explicit_creator(Some("13")));
        assert!(!room_requires_explicit_creator(Some("12.1")));
    }

    #[test]
    fn initial_power_levels_omits_sender_for_implicit_creator_versions() {
        let explicit =
            initial_power_levels("@_cumments_bot:example.com", "@owner:example.com", true);
        assert_eq!(explicit["users"]["@_cumments_bot:example.com"], 100);
        assert_eq!(explicit["users"]["@owner:example.com"], 100);

        let implicit =
            initial_power_levels("@_cumments_bot:example.com", "@owner:example.com", false);
        assert!(
            implicit["users"]
                .get("@_cumments_bot:example.com")
                .is_none()
        );
        assert_eq!(implicit["users"]["@owner:example.com"], 100);
    }

    #[test]
    fn capabilities_response_parses_default_room_version() {
        let caps: CapabilitiesResponse = serde_json::from_value(json!({
            "room_versions": {
                "default": "12",
                "available": { "11": "stable", "12": "stable" }
            }
        }))
        .expect("capabilities parse");
        assert_eq!(
            caps.room_versions.map(|r| r.default),
            Some("12".to_string())
        );

        let no_versions: CapabilitiesResponse =
            serde_json::from_value(json!({})).expect("empty capabilities parse");
        assert!(no_versions.room_versions.is_none());
    }
}

//! AppService MatrixDriver – uses `reqwest` to call the Matrix CS API
//! directly with the AppService `as_token`, supporting virtual users.

use crate::wire::{
    build_edit_body, build_message_body, build_redaction_body, comment_room_alias,
    comment_room_alias_localpart, format_txn_id, has_redact_power, has_state_power,
    initial_power_levels, metadata_matches, percent_encode, power_levels_with_admin,
    room_requires_explicit_creator, site_space_alias, site_space_alias_localpart,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::{
    identity::derive_guest_id_from_public_key,
    models::{PostSlug, RoomEventPage, SiteId},
    ports::{MatrixDriver, VirtualUserStore},
    protocol::ROOM_METADATA_EVENT_TYPE,
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
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
    #[serde(default)]
    available: HashMap<String, String>,
}

/// Upper bound for the joined-room cache. Membership changes are rare; when
/// the cap is hit the cache is reset and rebuilt from homeserver state.
const JOINED_CACHE_MAX: usize = 10_000;
const DISPLAY_NAME_CACHE_MAX: usize = 10_000;

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
    admin_id: String,
    virtual_user_store: Arc<dyn VirtualUserStore>,
    joined_cache: Mutex<HashSet<(String, String)>>,
    display_name_cache: Mutex<HashMap<String, String>>,
    /// Explicit room version from configuration, if any.
    room_version_override: Option<String>,
    /// Cached `m.room_versions.default` from `/capabilities`.
    default_room_version: Mutex<Option<String>>,
    /// Set once when `/capabilities` is unreachable or unusable for this
    /// homeserver (e.g. appservice credentials are rejected). Room-version
    /// preflight checks then skip further queries and warn only once per
    /// process, letting `createRoom` decide.
    capabilities_unavailable: Mutex<bool>,
}

impl AppServiceMatrixDriver {
    pub fn new(
        homeserver_url: String,
        as_token: String,
        server_name: String,
        sender_localpart: String,
        admin_id: String,
        virtual_user_store: Arc<dyn VirtualUserStore>,
        room_version: Option<String>,
    ) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow!("failed to build HTTP client: {e}"))?;
        Ok(Self {
            http_client,
            homeserver_url,
            as_token,
            server_name,
            sender_localpart,
            admin_id,
            virtual_user_store,
            joined_cache: Mutex::new(HashSet::new()),
            display_name_cache: Mutex::new(HashMap::new()),
            room_version_override: room_version,
            default_room_version: Mutex::new(None),
            capabilities_unavailable: Mutex::new(false),
        })
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

        let path = format!("_matrix/client/v3/rooms/{}/join", percent_encode(room_id));
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
            } else if status.is_server_error()
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || errcode == "M_LIMIT_EXCEEDED"
            {
                return Err(anyhow!(
                    "Virtual user {} join room {} failed with retryable error ({}): {}",
                    virtual_user,
                    room_id,
                    status,
                    error_body
                ));
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
            percent_encode(room_id),
            percent_encode(virtual_user)
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
    async fn ensure_display_name(&self, virtual_user: &str, display_name: &str) -> Result<()> {
        {
            let cache = self
                .display_name_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cache.get(virtual_user).map(String::as_str) == Some(display_name) {
                return Ok(());
            }
        }

        let path = format!(
            "_matrix/client/v3/profile/{}/displayname",
            percent_encode(virtual_user)
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, Some(virtual_user))
            .json(&serde_json::json!({ "displayname": display_name }))
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
        let mut cache = self
            .display_name_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cache.len() >= DISPLAY_NAME_CACHE_MAX && !cache.contains_key(virtual_user) {
            cache.clear();
        }
        cache.insert(virtual_user.to_owned(), display_name.to_owned());
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
            percent_encode(room_id),
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
    async fn fetch_room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/{}",
            percent_encode(room_id),
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
    async fn is_space_room(&self, room_id: &str) -> Result<bool> {
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
            percent_encode(room_id)
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
        if self.capabilities_unavailable() {
            return None;
        }

        let resp = match self
            .request(reqwest::Method::GET, "_matrix/client/v3/capabilities", None)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                self.note_capabilities_unavailable(&format!("query failed: {e:#}"));
                return None;
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            self.note_capabilities_unavailable(&format!("{status}: {error_body}"));
            return None;
        }

        let caps = match resp.json::<CapabilitiesResponse>().await {
            Ok(caps) => caps,
            Err(e) => {
                self.note_capabilities_unavailable(&format!("parse failed: {e:#}"));
                return None;
            }
        };
        let Some(version) = caps
            .room_versions
            .and_then(|r| (!r.default.is_empty()).then_some(r.default))
        else {
            self.note_capabilities_unavailable("no default room version advertised");
            return None;
        };
        *self
            .default_room_version
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(version.clone());
        Some(version)
    }

    /// Best-effort pre-check that the configured `room_version` is supported
    /// by the homeserver before creating a new room.
    ///
    /// The check only fails on a *definitive* answer: a successful
    /// `/capabilities` response that lists room versions without the
    /// configured one. Homeservers that reject appservice credentials (401)
    /// or do not advertise room versions are treated as unknown — the
    /// authoritative decision is left to `createRoom`, whose error surfaces
    /// in the intent's `last_error`.
    async fn validate_room_version_override(&self) -> Result<()> {
        let Some(version) = &self.room_version_override else {
            return Ok(());
        };
        if self.capabilities_unavailable() {
            return Ok(());
        }
        let resp = self
            .request(reqwest::Method::GET, "_matrix/client/v3/capabilities", None)
            .send()
            .await;
        let resp = match resp {
            Ok(resp) => resp,
            Err(e) => {
                self.note_capabilities_unavailable(&format!("query failed: {e}"));
                return Ok(());
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            self.note_capabilities_unavailable(&format!("{status}: {error_body}"));
            return Ok(());
        }
        let caps: CapabilitiesResponse = match resp.json().await {
            Ok(caps) => caps,
            Err(e) => {
                self.note_capabilities_unavailable(&format!("parse failed: {e}"));
                return Ok(());
            }
        };
        let available = caps.room_versions.map(|r| r.available).unwrap_or_default();
        if available.is_empty() {
            self.note_capabilities_unavailable("no room versions advertised");
            return Ok(());
        }
        room_version_decision(version, &available).map_err(anyhow::Error::msg)
    }

    /// Whether `/capabilities` has already been marked unusable for this
    /// process.
    fn capabilities_unavailable(&self) -> bool {
        *self
            .capabilities_unavailable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Marks `/capabilities` unusable for this process and warns exactly once.
    fn note_capabilities_unavailable(&self, reason: &str) {
        let mut guard = self
            .capabilities_unavailable
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *guard {
            return;
        }
        *guard = true;
        warn!(
            "Homeserver capabilities are unavailable for this process ({reason}); \
             skipping room-version checks and letting createRoom decide until restart"
        );
    }

    /// Read a room's current `m.room.power_levels` content, if any.
    async fn get_power_levels(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/m.room.power_levels",
            percent_encode(room_id)
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
            percent_encode(room_id)
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

    /// Whether the AS sender can redact other users' events in the room.
    async fn sender_can_redact(&self, room_id: &str) -> Result<bool> {
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
            Some(power_levels) => has_redact_power(&power_levels, &self.sender_user_id()),
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
            Ok(true) => {}
            Ok(false) => {
                return Err(anyhow!(
                    "Refusing to adopt room {}: AS sender cannot write state \
                 (not a member or insufficient power)",
                    room_id
                ));
            }
            Err(e) => {
                return Err(anyhow!(
                    "Cannot verify governance of room {} before adoption: {:#}",
                    room_id,
                    e
                ));
            }
        }
        if !self.sender_can_redact(room_id).await? {
            return Err(anyhow!(
                "Refusing to adopt room {}: AS sender cannot meet the room's \
                 redact threshold (delete intents would fail)",
                room_id
            ));
        }
        Ok(())
    }

    /// Unified adoption gate for every recovery path. Always verifies the AS
    /// sender can govern the room before accepting it, then repairs metadata
    /// when necessary and ensures the operator admin has power.
    async fn adopt_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: Option<&PostSlug>,
        require_space: bool,
    ) -> Result<()> {
        if require_space && !self.is_space_room(room_id).await? {
            anyhow::bail!(
                "Refusing to adopt room {} as a site space: not created as m.space",
                room_id
            );
        }
        self.ensure_room_adoptable(room_id).await?;
        if !self
            .room_metadata_matches(room_id, site_id, post_slug)
            .await?
        {
            self.set_room_metadata(room_id, site_id, post_slug).await?;
        }
        self.ensure_admin_strict(room_id).await?;
        Ok(())
    }

    /// Resolve a room ID from its alias via the homeserver directory.
    async fn resolve_room_by_alias(&self, alias: &str) -> Result<Option<String>> {
        let path = format!("_matrix/client/v3/directory/room/{}", percent_encode(alias));
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
        Ok(match self.fetch_room_metadata(room_id).await? {
            Some(meta) => metadata_matches(&meta, site_id.as_str(), post_slug.map(|s| s.as_str())),
            None => false,
        })
    }

    /// Idempotently link a comment room into its site Space (`m.space.child`
    /// on the Space, `m.space.parent` on the room). Errors are returned so
    /// the caller retries the intent, which re-enters `ensure_comment_room`
    /// and re-links the room.
    async fn link_room_to_space(&self, space_id: &str, room_id: &str) -> Result<()> {
        let child_content = serde_json::json!({ "via": [self.server_name] });
        let space_path = format!(
            "_matrix/client/v3/rooms/{}/state/m.space.child/{}",
            percent_encode(space_id),
            percent_encode(room_id)
        );
        let child_resp = self
            .request(reqwest::Method::PUT, &space_path, None)
            .json(&child_content)
            .send()
            .await;
        match child_resp {
            Err(e) => {
                return Err(anyhow!(
                    "Failed to link room {} to space {}: {}",
                    room_id,
                    space_id,
                    e
                ));
            }
            Ok(resp) if !resp.status().is_success() => {
                let status = resp.status();
                let error_body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "Failed to link room {} to space {} ({}): {}",
                    room_id,
                    space_id,
                    status,
                    error_body
                ));
            }
            _ => {}
        }

        let parent_path = format!(
            "_matrix/client/v3/rooms/{}/state/m.space.parent/{}",
            percent_encode(room_id),
            percent_encode(space_id)
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
                return Err(anyhow!(
                    "Failed to link space {} as parent of room {}: {}",
                    space_id,
                    room_id,
                    e
                ));
            }
            Ok(resp) if !resp.status().is_success() => {
                let status = resp.status();
                let error_body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "Failed to link space {} as parent of room {} ({}): {}",
                    space_id,
                    room_id,
                    status,
                    error_body
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// Grant the operator admin power in a room, propagating failures so
    /// adoption cannot silently proceed with an unmoderated room.
    #[instrument(skip(self))]
    async fn ensure_admin_strict(&self, room_id: &str) -> Result<()> {
        let updated = match self.get_power_levels(room_id).await {
            Ok(Some(content)) => match power_levels_with_admin(&content, &self.admin_id) {
                Some(updated) => updated,
                None => return Ok(()), // admin already has admin power
            },
            // No power-levels event: room defaults apply, so create one that
            // grants the admin power.
            Ok(None) => serde_json::json!({ "users": { self.admin_id.clone(): 100 } }),
            Err(e) => {
                return Err(anyhow!(
                    "Failed to read power levels for room {}: {:#}",
                    room_id,
                    e
                ));
            }
        };

        self.write_power_levels(room_id, &updated).await?;
        info!("Granted admin power in room {}", room_id);
        Ok(())
    }
}

/// Pure decision for a `/capabilities` room-version list.
///
/// An empty list means the homeserver did not advertise room versions: the
/// version can neither be confirmed nor ruled out, so the caller proceeds and
/// lets `createRoom` decide. A non-empty list is definitive — the version
/// either is supported or the configuration is invalid.
fn room_version_decision(version: &str, available: &HashMap<String, String>) -> Result<(), String> {
    if available.is_empty() {
        return Ok(());
    }
    if available.contains_key(version) {
        Ok(())
    } else {
        Err(format!(
            "configured room_version `{version}` is not supported by homeserver /capabilities \
             (available: {})",
            available.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    }
}

#[async_trait]
impl MatrixDriver for AppServiceMatrixDriver {
    #[instrument(skip(self), fields(site_id = %site_id.as_str()))]
    async fn create_site_space(&self, site_id: &SiteId) -> Result<String> {
        self.validate_room_version_override().await?;
        let site_id_str = site_id.as_str();
        let alias_localpart = site_space_alias_localpart(site_id_str);

        info!("Creating new space for site {} via AppService", site_id_str);

        // When the homeserver's default room version cannot be determined,
        // the explicit-creator policy is a guess: try the conservative
        // pre-v12 policy first, then retry once with the opposite policy.
        // A failed createRoom leaves no residue, so the retry is safe.
        let version = self.effective_room_version().await;
        let mut creator_policies = vec![room_requires_explicit_creator(version.as_deref())];
        if version.is_none() {
            creator_policies.push(!creator_policies[0]);
        }
        let mut last_error: Option<anyhow::Error> = None;

        for include_sender in creator_policies {
            let power_levels =
                initial_power_levels(&self.sender_user_id(), &self.admin_id, include_sender);
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
                "invite": [self.admin_id.clone()],
                "preset": "public_chat",
            });
            if let Some(version) = &self.room_version_override {
                body["room_version"] = serde_json::json!(version);
            }

            let resp = match self
                .request(reqwest::Method::POST, "_matrix/client/v3/createRoom", None)
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    last_error = Some(anyhow!("createRoom request failed: {e}"));
                    continue;
                }
            };
            if resp.status().is_success() {
                let data: CreateRoomResponse = resp
                    .json()
                    .await
                    .map_err(|e| anyhow!("Failed to parse createRoom response: {e}"))?;
                return Ok(data.room_id);
            }
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            last_error = Some(anyhow!("createRoom failed ({status}): {error_body}"));
            warn!("createRoom failed ({status}): {error_body}. Trying alias recovery.");
        }

        // Recovery: after a local DB reset (or a partial previous run) the
        // space may already exist under our exclusive alias. Adopt it.
        let alias = site_space_alias(&self.server_name, site_id_str);
        match self.resolve_room_by_alias(&alias).await? {
            Some(room_id) => {
                self.adopt_room(&room_id, site_id, None, true).await?;
                info!(
                    "Recovered existing site space {} via alias {}",
                    room_id, alias
                );
                Ok(room_id)
            }
            None => Err(last_error.unwrap_or_else(|| {
                anyhow!("Failed to create site space; no createRoom attempt was made")
            })),
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
            && let Ok(Some(meta)) = self.fetch_room_metadata(candidate).await
            && metadata_matches(&meta, site_id.as_str(), Some(post_slug.as_str()))
        {
            self.adopt_room(candidate, site_id, Some(post_slug), false)
                .await?;
            target_room_id = Some(candidate.to_string());
        }

        // ── PHASE 0.5: ALIAS RECOVERY (cold local registry) ──
        if target_room_id.is_none() {
            let alias = comment_room_alias(&self.server_name, site_id.as_str(), post_slug.as_str());
            if let Some(room_id) = self.resolve_room_by_alias(&alias).await? {
                self.adopt_room(&room_id, site_id, Some(post_slug), false)
                    .await?;
                info!(
                    "Recovered existing comment room {} via alias {}",
                    room_id, alias
                );
                target_room_id = Some(room_id);
            }
        }

        let room_id = if let Some(id) = target_room_id {
            id
        } else {
            self.validate_room_version_override().await?;
            // ── PHASE 1: Create new comment room ──
            info!("No matching room found. Creating new comment room via AppService.");
            let alias_localpart =
                comment_room_alias_localpart(site_id.as_str(), post_slug.as_str());

            // Same conservative-then-opposite retry as create_site_space:
            // when the homeserver default is unknown, a failed createRoom is
            // retried once with the other explicit-creator policy.
            let version = self.effective_room_version().await;
            let mut creator_policies = vec![room_requires_explicit_creator(version.as_deref())];
            if version.is_none() {
                creator_policies.push(!creator_policies[0]);
            }
            let mut last_error: Option<anyhow::Error> = None;
            let mut created_room_id: Option<String> = None;

            for include_sender in creator_policies {
                let power_levels =
                    initial_power_levels(&self.sender_user_id(), &self.admin_id, include_sender);
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
                    "invite": [self.admin_id.clone()],
                    "preset": "public_chat",
                });
                if let Some(version) = &self.room_version_override {
                    body["room_version"] = serde_json::json!(version);
                }

                let resp = match self
                    .request(reqwest::Method::POST, "_matrix/client/v3/createRoom", None)
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(resp) => resp,
                    Err(e) => {
                        last_error = Some(anyhow!("createRoom request failed: {e}"));
                        continue;
                    }
                };
                if resp.status().is_success() {
                    let data: CreateRoomResponse = resp
                        .json()
                        .await
                        .map_err(|e| anyhow!("Failed to parse createRoom response: {e}"))?;
                    created_room_id = Some(data.room_id);
                    break;
                }
                let status = resp.status();
                let error_body = resp.text().await.unwrap_or_default();
                last_error = Some(anyhow!("createRoom failed ({status}): {error_body}"));
                warn!("createRoom failed ({status}): {error_body}. Trying alias recovery.");
            }

            match created_room_id {
                Some(room_id) => room_id,
                None => {
                    // Recovery: another attempt may have won the race, or the
                    // room already exists after a DB reset.
                    let alias =
                        comment_room_alias(&self.server_name, site_id.as_str(), post_slug.as_str());
                    match self.resolve_room_by_alias(&alias).await? {
                        Some(room_id) => {
                            self.adopt_room(&room_id, site_id, Some(post_slug), false)
                                .await?;
                            info!(
                                "Recovered comment room {} after createRoom failure via alias {}",
                                room_id, alias
                            );
                            self.link_room_to_space(space_id, &room_id).await?;
                            room_id
                        }
                        None => {
                            return Err(last_error.unwrap_or_else(|| {
                                anyhow!(
                                    "Failed to create comment room; no createRoom attempt was made"
                                )
                            }));
                        }
                    }
                }
            }
        };

        // Keep the room linked to its Space (idempotent, best-effort).
        self.link_room_to_space(space_id, &room_id).await?;

        Ok(room_id)
    }

    #[instrument(skip(self))]
    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        display_name: &str,
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
        if let Err(e) = self.ensure_display_name(&virtual_user, display_name).await {
            warn!("Failed to set display name for {}: {:#}", virtual_user, e);
        }

        // 3. Send the message as the virtual user
        let message_body = build_message_body(
            content,
            display_name,
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
            percent_encode(room_id),
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
        display_name: &str,
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
        if let Err(e) = self.ensure_display_name(&virtual_user, display_name).await {
            warn!("Failed to set display name for {}: {:#}", virtual_user, e);
        }

        // 3. Send m.replace as the virtual user
        let message_body = build_edit_body(
            event_id,
            new_content,
            display_name,
            author_public_key,
            author_signature,
            author_challenge,
            &guest_id,
            intent_id,
        );

        let txn_id = self.txn_id("update", intent_id);
        let path = format!(
            "_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            percent_encode(room_id),
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
            percent_encode(room_id),
            percent_encode(event_id),
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
            percent_encode(room_id),
            percent_encode(event_id)
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
            percent_encode(room_id),
            limit
        );
        if let Some(from) = from {
            path.push_str(&format!("&from={}", percent_encode(from)));
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
            has_more: data.start != data.end,
        })
    }

    #[instrument(skip(self))]
    async fn get_joined_rooms(&self) -> Result<Vec<String>> {
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

    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        self.fetch_room_metadata(room_id).await
    }

    #[instrument(skip(self))]
    async fn get_room_canonical_alias(&self, room_id: &str) -> Result<Option<String>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/state/m.room.canonical_alias",
            percent_encode(room_id)
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

    async fn ensure_admin(&self, room_id: &str) {
        if let Err(e) = self.ensure_admin_strict(room_id).await {
            warn!("Failed to ensure admin power in room {}: {:#}", room_id, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn room_version_decision_accepts_supported_and_empty_lists() {
        let mut available = HashMap::new();
        available.insert("12".to_string(), "stable".to_string());
        assert!(room_version_decision("12", &available).is_ok());
        assert!(
            room_version_decision("11", &available).is_err(),
            "a definitive list without the version must fail"
        );

        let empty = HashMap::new();
        assert!(
            room_version_decision("12", &empty).is_ok(),
            "no advertised versions must be treated as unknown, not unsupported"
        );
    }

    #[test]
    fn room_version_decision_error_lists_available_versions() {
        let mut available = HashMap::new();
        available.insert("10".to_string(), "stable".to_string());
        available.insert("11".to_string(), "stable".to_string());
        let error = room_version_decision("12", &available).expect_err("must reject");
        assert!(error.contains("`12` is not supported"));
        assert!(
            error.contains("10") && error.contains("11"),
            "error must list available versions: {error}"
        );
    }
}

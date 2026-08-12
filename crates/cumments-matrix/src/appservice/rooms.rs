//! Room lifecycle: creation, adoption, governance, metadata and space
//! linking.

use super::*;
use crate::wire::{
    comment_room_alias, comment_room_alias_localpart, has_redact_power, has_state_power,
    initial_power_levels, is_implicit_creator, metadata_matches, percent_encode,
    power_levels_with_admin, room_requires_explicit_creator, site_space_alias,
    site_space_alias_localpart,
};
use anyhow::{Result, anyhow};
use cumments_core::{
    models::{PostSlug, SiteId},
    protocol::ROOM_METADATA_EVENT_TYPE,
};
use serde::Deserialize;
use tracing::{info, instrument, warn};

#[derive(Deserialize)]
struct CreateRoomResponse {
    room_id: String,
}

#[derive(Deserialize)]
struct ResolveAliasResponse {
    room_id: String,
}

/// The first event(s) of a room's timeline as returned by `/messages`.
#[derive(Deserialize)]
struct CreateEventPage {
    #[serde(default)]
    chunk: Vec<serde_json::Value>,
}

/// The `m.room.create` event envelope. Since room version 11 the event
/// content has no `creator` field: the sender is the room creator.
#[derive(Deserialize)]
struct RoomCreateEvent {
    sender: String,
    content: RoomCreateEventContent,
}

#[derive(Deserialize)]
struct RoomCreateEventContent {
    /// The room version; absent means version 1.
    #[serde(default)]
    room_version: Option<String>,
    /// Room version 12+: additional user IDs granted creator power.
    #[serde(default)]
    additional_creators: Option<Vec<String>>,
}

impl AppServiceMatrixDriver {
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
    /// `content.type: "m.space"`).
    async fn is_space_room(&self, room_id: &str) -> Result<bool> {
        Ok(match self.get_room_create_state(room_id).await? {
            // `/state` returns the event's *content*: the space marker is a
            // top-level `type` key there, not an `m.room.create` envelope.
            Some(create) => create.get("type").and_then(|v| v.as_str()) == Some("m.space"),
            None => false,
        })
    }

    /// Fetch the *content* of the room's `m.room.create` state event (the CS
    /// API returns the content, e.g. `{"room_version":"12"}`), if the room
    /// exists.
    async fn get_room_create_state(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
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

    /// Fetch the room's `m.room.create` event envelope (sender + content)
    /// from the first event of the room's timeline, if the AS sender can see
    /// it.
    ///
    /// `GET /messages?dir=f` without `from` returns events from the first
    /// visible event (spec v1.3+), which for a room created by the AS sender
    /// is the create event. When the create event is not the first visible
    /// event (e.g. history visibility hides it from a later joiner), the CS
    /// API cannot expose its sender, so `None` is returned and callers fall
    /// back to the power-levels check.
    #[instrument(skip(self))]
    async fn fetch_create_event(&self, room_id: &str) -> Result<Option<RoomCreateEvent>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/messages?dir=f&limit=1",
            percent_encode(room_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to fetch room history: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to fetch create event for {} ({}): {}",
                room_id,
                status,
                error_body
            ));
        }

        let data: CreateEventPage = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse messages response: {}", e))?;
        let Some(event) = data.chunk.into_iter().next() else {
            return Ok(None);
        };
        if event.get("type").and_then(|v| v.as_str()) != Some("m.room.create") {
            return Ok(None);
        }
        serde_json::from_value(event)
            .map(Some)
            .map_err(|e| anyhow!("Failed to parse create event for {}: {}", room_id, e))
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

    /// Whether the AS sender is a room creator under the v12+ implicit-power
    /// rules and is currently joined to the room.
    async fn sender_has_implicit_creator_power(
        &self,
        room_id: &str,
        create: Option<&RoomCreateEvent>,
    ) -> bool {
        let Some(create) = create else {
            return false;
        };
        let sender = self.sender_user_id();
        is_implicit_creator(
            create.content.room_version.as_deref(),
            &create.sender,
            create.content.additional_creators.as_deref(),
            &sender,
        ) && self.is_joined(room_id, &sender).await
    }

    /// Whether the AS sender can write state events in the room. Reading the
    /// power levels doubles as a membership check: non-members get an error.
    async fn sender_can_write_state(&self, room_id: &str, implicit_creator: bool) -> Result<bool> {
        // Room versions 12+ give the creator immutable infinite power without
        // listing them in `m.room.power_levels.users`; account for that
        // before falling back to the power-levels check.
        if implicit_creator {
            return Ok(true);
        }
        Ok(match self.get_power_levels(room_id).await? {
            Some(power_levels) => has_state_power(&power_levels, &self.sender_user_id()),
            // No power-levels event: Matrix defaults apply and leave the
            // sender below the state-writing threshold, so treat as unable.
            None => false,
        })
    }

    /// Whether the AS sender can redact other users' events in the room.
    async fn sender_can_redact(&self, room_id: &str, implicit_creator: bool) -> Result<bool> {
        if implicit_creator {
            return Ok(true);
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
        let create = self.fetch_create_event(room_id).await?;
        let implicit_creator = self
            .sender_has_implicit_creator_power(room_id, create.as_ref())
            .await;
        match self.sender_can_write_state(room_id, implicit_creator).await {
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
        if !self.sender_can_redact(room_id, implicit_creator).await? {
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

    #[instrument(skip(self), fields(site_id = %site_id.as_str()))]
    pub(super) async fn create_site_space_impl(&self, site_id: &SiteId) -> Result<String> {
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
    pub(super) async fn ensure_comment_room_impl(
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

    pub(super) async fn get_room_metadata_impl(
        &self,
        room_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        self.fetch_room_metadata(room_id).await
    }

    #[instrument(skip(self))]
    pub(super) async fn get_room_canonical_alias_impl(
        &self,
        room_id: &str,
    ) -> Result<Option<String>> {
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

    pub(super) async fn ensure_admin_impl(&self, room_id: &str) {
        if let Err(e) = self.ensure_admin_strict(room_id).await {
            warn!("Failed to ensure admin power in room {}: {:#}", room_id, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn create_event_parses_v12_envelope_without_creator() {
        let event: RoomCreateEvent = serde_json::from_value(json!({
            "type": "m.room.create",
            "sender": "@_cumments_bot:example.com",
            "content": { "room_version": "12" }
        }))
        .expect("parse create event");
        assert_eq!(event.sender, "@_cumments_bot:example.com");
        assert_eq!(event.content.room_version.as_deref(), Some("12"));
        assert!(event.content.additional_creators.is_none());
    }

    #[test]
    fn create_event_parses_space_type_and_additional_creators() {
        let event: RoomCreateEvent = serde_json::from_value(json!({
            "type": "m.room.create",
            "sender": "@_cumments_bot:example.com",
            "content": {
                "type": "m.space",
                "room_version": "12",
                "additional_creators": ["@someone:example.com"]
            }
        }))
        .expect("parse create event");
        assert_eq!(
            event.content.additional_creators.as_deref(),
            Some(&["@someone:example.com".to_string()][..])
        );
        assert_eq!(event.content.room_version.as_deref(), Some("12"));
    }

    #[tokio::test]
    async fn v12_creator_shortcut_uses_create_event_sender() {
        let server = MockServer::start().await;
        mount_create_events(
            &server,
            json!({
                "type": "m.room.create",
                "sender": "@_cumments_bot:example.com",
                "content": { "room_version": "12" }
            }),
        )
        .await;
        mount_joined_membership(&server, 1).await;
        // The implicit creator must not need (or look up) power levels.
        mount_power_levels(&server, json!({}), 0).await;

        let driver = test_driver(&server);
        driver
            .ensure_room_adoptable(ROOM_ID)
            .await
            .expect("v12 creator must be adoptable");
        server.verify().await;
    }

    #[tokio::test]
    async fn v12_additional_creators_grant_implicit_power() {
        let server = MockServer::start().await;
        mount_create_events(
            &server,
            json!({
                "type": "m.room.create",
                "sender": "@someone:example.com",
                "content": {
                    "room_version": "12",
                    "additional_creators": ["@_cumments_bot:example.com"]
                }
            }),
        )
        .await;
        mount_joined_membership(&server, 1).await;
        mount_power_levels(&server, json!({}), 0).await;

        let driver = test_driver(&server);
        driver
            .ensure_room_adoptable(ROOM_ID)
            .await
            .expect("additional creator must be adoptable");
        server.verify().await;
    }

    #[tokio::test]
    async fn unverifiable_create_event_falls_back_to_power_levels() {
        let server = MockServer::start().await;
        // History visibility hides the create event, so the first visible
        // event is a message and the creator cannot be confirmed.
        mount_create_events(
            &server,
            json!({
                "type": "m.room.message",
                "sender": "@someone:example.com",
                "content": { "body": "hi" }
            }),
        )
        .await;
        mount_power_levels(
            &server,
            json!({ "users": { "@_cumments_bot:example.com": 100 } }),
            2,
        )
        .await;

        let driver = test_driver(&server);
        driver
            .ensure_room_adoptable(ROOM_ID)
            .await
            .expect("power-level fallback must decide");
        server.verify().await;
    }

    #[tokio::test]
    async fn pre_v12_creator_does_not_skip_power_levels() {
        let server = MockServer::start().await;
        mount_create_events(
            &server,
            json!({
                "type": "m.room.create",
                "sender": "@_cumments_bot:example.com",
                "content": { "room_version": "11" }
            }),
        )
        .await;
        mount_power_levels(&server, json!({ "users": {} }), 1).await;

        let driver = test_driver(&server);
        let error = driver
            .ensure_room_adoptable(ROOM_ID)
            .await
            .expect_err("pre-v12 creator without power must be refused");
        assert!(
            error.to_string().contains("cannot write state"),
            "unexpected error: {error}"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn is_space_room_reads_type_from_create_state_content() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(CREATE_STATE_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "room_version": "12",
                "type": "m.space"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        assert!(driver.is_space_room(ROOM_ID).await.unwrap());
        server.verify().await;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(CREATE_STATE_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "room_version": "12"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        assert!(!driver.is_space_room(ROOM_ID).await.unwrap());
        server.verify().await;
    }
}

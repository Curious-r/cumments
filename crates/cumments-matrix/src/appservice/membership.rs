//! Virtual-user identity, membership and joined-room tracking.

use super::*;
use crate::wire::percent_encode;
use anyhow::{Result, anyhow};
use cumments_core::models::SiteId;
use serde::Deserialize;
use tracing::{instrument, warn};

/// Upper bound for the joined-room cache. Membership changes are rare; when
/// the cap is hit the cache is reset and rebuilt from homeserver state.
const JOINED_CACHE_MAX: usize = 10_000;
const DISPLAY_NAME_CACHE_MAX: usize = 10_000;

#[derive(Deserialize)]
struct JoinedRoomsResponse {
    joined_rooms: Vec<String>,
}

#[derive(Deserialize)]
struct JoinedMembersResponse {
    joined: std::collections::HashMap<String, serde_json::Value>,
}

impl AppServiceMatrixDriver {
    /// Resolve the virtual user ID for a given author public key.
    pub(super) async fn resolve_virtual_user(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<String> {
        self.virtual_user_store
            .get_or_create_virtual_user(author_public_key, site_id, &self.server_name)
            .await
    }

    /// Ensure a virtual user is joined to a room.
    pub(super) async fn ensure_joined(&self, room_id: &str, virtual_user: &str) -> Result<()> {
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

    /// Makes the AS sender leave a room. A room the sender is not in (or
    /// that no longer exists) counts as already left.
    pub(super) async fn leave_room_impl(&self, room_id: &str) -> Result<()> {
        let path = format!("_matrix/client/v3/rooms/{}/leave", percent_encode(room_id));
        let resp = self
            .request(reqwest::Method::POST, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Leave request failed: {}", e))?;

        match resp.status().as_u16() {
            200 | 404 => Ok(()),
            403 => {
                let body = resp.text().await.unwrap_or_default();
                let errcode = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("errcode")
                            .and_then(|e| e.as_str())
                            .map(str::to_owned)
                    });
                if errcode.as_deref() == Some("M_FORBIDDEN") {
                    // Already left / never joined.
                    Ok(())
                } else {
                    Err(anyhow!("Leaving room {room_id} was forbidden: {body}"))
                }
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(anyhow!("Leaving room {room_id} failed ({status}): {body}"))
            }
        }
    }

    /// Makes a specific AS-managed user (e.g. a guest virtual user) leave a
    /// room. A user who is not in the room counts as already left.
    pub(super) async fn leave_room_as_impl(&self, room_id: &str, user_id: &str) -> Result<()> {
        let path = format!("_matrix/client/v3/rooms/{}/leave", percent_encode(room_id));
        let resp = self
            .request(reqwest::Method::POST, &path, Some(user_id))
            .send()
            .await
            .map_err(|e| anyhow!("Leave request failed: {}", e))?;

        match resp.status().as_u16() {
            200 | 404 => Ok(()),
            403 => {
                let body = resp.text().await.unwrap_or_default();
                let errcode = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("errcode")
                            .and_then(|e| e.as_str())
                            .map(str::to_owned)
                    });
                if errcode.as_deref() == Some("M_FORBIDDEN") {
                    Ok(())
                } else {
                    Err(anyhow!(
                        "Leaving room {room_id} as {user_id} was forbidden: {body}"
                    ))
                }
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(anyhow!(
                    "Leaving room {room_id} as {user_id} failed ({status}): {body}"
                ))
            }
        }
    }

    /// Makes the AS sender join a room, typically to accept a claim-DM
    /// invite after the conditional auto-join gate passes. A room the sender
    /// is already in counts as joined.
    pub(super) async fn join_room_impl(&self, room_id: &str) -> Result<()> {
        let path = format!("_matrix/client/v3/rooms/{}/join", percent_encode(room_id));
        let resp = self
            .request(reqwest::Method::POST, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Join request failed: {}", e))?;

        match resp.status().as_u16() {
            200 => Ok(()),
            403 => {
                let body = resp.text().await.unwrap_or_default();
                let errcode = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("errcode")
                            .and_then(|e| e.as_str())
                            .map(str::to_owned)
                    });
                if errcode.as_deref() == Some("M_FORBIDDEN")
                    && self.is_joined(room_id, &self.sender_user_id()).await
                {
                    Ok(())
                } else {
                    Err(anyhow!("Joining room {room_id} was forbidden: {body}"))
                }
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(anyhow!("Joining room {room_id} failed ({status}): {body}"))
            }
        }
    }

    /// Invites a real Matrix user to a room as the AS sender. Users who are
    /// already joined (including a join racing the invite) are a successful
    /// no-op.
    pub(super) async fn invite_user_impl(&self, room_id: &str, user_id: &str) -> Result<()> {
        if self.is_joined(room_id, user_id).await {
            return Ok(());
        }

        let path = format!("_matrix/client/v3/rooms/{}/invite", percent_encode(room_id));
        let resp = self
            .request(reqwest::Method::POST, &path, None)
            .json(&serde_json::json!({ "user_id": user_id }))
            .send()
            .await
            .map_err(|e| anyhow!("Invite request failed: {}", e))?;

        if resp.status().is_success() {
            return Ok(());
        }

        // A user may have joined between the membership check and the invite;
        // the homeserver then reports M_FORBIDDEN. Confirm the join before
        // surfacing an error.
        if self.is_joined(room_id, user_id).await {
            return Ok(());
        }
        let status = resp.status();
        let error_body = resp.text().await.unwrap_or_default();
        Err(anyhow!(
            "Invite {user_id} to {room_id} failed ({status}): {error_body}"
        ))
    }

    /// Whether the virtual user has `join` membership in the room, queried as
    /// the AS sender. Errors are logged and treated as "not joined" so the
    /// subsequent `/join` (or the final send) remains the authority.
    pub(super) async fn is_joined(&self, room_id: &str, virtual_user: &str) -> bool {
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

    pub(super) fn invalidate_joined(&self, cache_key: &(String, String)) {
        self.joined_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(cache_key);
    }

    /// Best-effort: keep the virtual user's display name in sync with the
    /// commenter's display name so Matrix clients show it instead of
    /// the localpart. Failures only warn; the message send still proceeds.
    pub(super) async fn ensure_display_name(
        &self,
        virtual_user: &str,
        display_name: &str,
    ) -> Result<()> {
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

    /// Sets or removes the avatar on a virtual user's global profile.
    ///
    /// The update carries `m.propagate_to: "all"` (MSC4466) so the
    /// homeserver emits fresh `m.room.member` events in every joined room;
    /// without that propagation the avatar would never reach the projector
    /// (avatars have no event-content fallback, unlike display names).
    #[instrument(skip(self))]
    pub(super) async fn set_avatar_url_impl(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        avatar_url: Option<&str>,
    ) -> Result<()> {
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        let path = format!(
            "_matrix/client/v3/profile/{}/avatar_url",
            percent_encode(&virtual_user)
        );
        let resp = match avatar_url {
            Some(avatar_url) => self
                .request(reqwest::Method::PUT, &path, Some(&virtual_user))
                .json(&serde_json::json!({
                    "avatar_url": avatar_url,
                    "m.propagate_to": "all",
                }))
                .send()
                .await
                .map_err(|e| anyhow!("set avatar request failed: {}", e))?,
            None => self
                .request(reqwest::Method::DELETE, &path, Some(&virtual_user))
                .send()
                .await
                .map_err(|e| anyhow!("delete avatar request failed: {}", e))?,
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "set avatar for {} failed ({}): {}",
                virtual_user,
                status,
                error_body
            ));
        }
        Ok(())
    }

    #[instrument(skip(self))]
    pub(super) async fn get_joined_rooms_impl(&self) -> Result<Vec<String>> {
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

    /// Lists the joined member MXIDs of a room.
    pub(super) async fn get_joined_members_impl(&self, room_id: &str) -> Result<Vec<String>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/joined_members",
            percent_encode(room_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to list joined members: {}", e))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to list joined members for {room_id} ({status}): {error_body}"
            ));
        }

        let data: JoinedMembersResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse joined_members response: {}", e))?;
        Ok(data.joined.into_keys().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::test_driver;
    use super::*;
    use cumments_core::ports::MatrixDriver;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const AVATAR_PROFILE_PATH: &str =
        "/_matrix/client/v3/profile/%40_cumments_my-blog_pubkey%3Aexample.com/avatar_url";

    #[tokio::test]
    async fn set_avatar_url_updates_the_virtual_user_profile_and_propagates() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(AVATAR_PROFILE_PATH))
            .and(query_param(
                "user_id",
                "@_cumments_my-blog_pubkey:example.com",
            ))
            .and(body_partial_json(json!({
                "avatar_url": "mxc://example.com/avatar",
                "m.propagate_to": "all",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        driver
            .set_avatar_url(
                "pubkey",
                &SiteId::from("my-blog"),
                Some("mxc://example.com/avatar"),
            )
            .await
            .expect("set avatar should succeed");
        server.verify().await;
    }

    #[tokio::test]
    async fn delete_avatar_url_removes_the_profile_field() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path(AVATAR_PROFILE_PATH))
            .and(query_param(
                "user_id",
                "@_cumments_my-blog_pubkey:example.com",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        driver
            .set_avatar_url("pubkey", &SiteId::from("my-blog"), None)
            .await
            .expect("delete avatar should succeed");
        server.verify().await;
    }
}

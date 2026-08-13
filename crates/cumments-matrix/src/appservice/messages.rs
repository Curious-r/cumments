//! Sending, editing, redacting and reading room messages.

use super::*;
use crate::wire::{
    build_edit_body, build_location_body, build_media_body, build_message_body,
    build_poll_vote_body, build_reaction_body, build_redaction_body, percent_encode,
};
use anyhow::{Result, anyhow};
use cumments_core::{
    identity::derive_guest_id_from_public_key,
    models::{CommentMedia, RoomEventPage, SiteId},
};
use serde::Deserialize;
use tracing::{instrument, warn};

#[derive(Deserialize)]
struct SendEventResponse {
    event_id: String,
}

#[derive(Deserialize)]
struct MessagesResponse {
    start: String,
    end: String,
    chunk: Vec<serde_json::Value>,
}

impl AppServiceMatrixDriver {
    /// Deletes one media item from the homeserver, mirroring the media
    /// proxy's best-effort sweep. Foreign servers and refused deletions
    /// report `false` so the local record is kept.
    pub(super) async fn delete_media_impl(&self, server: &str, media_id: &str) -> Result<bool> {
        if server != self.server_name {
            return Ok(false);
        }
        let path = format!("_matrix/media/v3/delete/{server}/{media_id}");
        let resp = self
            .request(reqwest::Method::DELETE, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("media deletion request failed: {}", e))?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    #[instrument(skip(self))]
    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
    pub(super) async fn post_message_impl(
        &self,
        room_id: &str,
        content: &str,
        media: Option<&CommentMedia>,
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
        submission_id: Option<i64>,
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
        let message_body = match media {
            Some(media) => build_media_body(
                media,
                display_name,
                author_public_key,
                author_signature,
                author_challenge,
                &guest_id,
                submission_id,
            ),
            None => build_message_body(
                content,
                display_name,
                author_public_key,
                author_signature,
                author_challenge,
                &guest_id,
                submission_id,
                reply_to,
                reply_to_body,
                reply_to_sender,
            ),
        };

        let txn_id = self.txn_id("post", submission_id);
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
                return Err(room_gone(
                    room_id,
                    format!("Failed to post message ({status}): {error_body}"),
                ));
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
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn react_message_impl(
        &self,
        room_id: &str,
        target_event_id: &str,
        key: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
    ) -> Result<()> {
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        self.ensure_joined(room_id, &virtual_user).await?;
        let guest_id = derive_guest_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow!("invalid author public key"))?;
        let body = build_reaction_body(
            key,
            target_event_id,
            author_public_key,
            author_signature,
            author_challenge,
            &guest_id,
        );
        let txn_id = self.txn_id("react", None);
        let path = format!(
            "_matrix/client/v3/rooms/{}/send/m.reaction/{}",
            percent_encode(room_id),
            txn_id
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, Some(&virtual_user))
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("reactMessage request failed: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to react ({}): {}", status, error_body));
        }
        Ok(())
    }

    #[instrument(skip(self))]
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn vote_poll_impl(
        &self,
        room_id: &str,
        poll_event_id: &str,
        answer_id: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
    ) -> Result<()> {
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        self.ensure_joined(room_id, &virtual_user).await?;
        let guest_id = derive_guest_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow!("invalid author public key"))?;
        let body = build_poll_vote_body(
            poll_event_id,
            answer_id,
            author_public_key,
            author_signature,
            author_challenge,
            &guest_id,
        );
        let txn_id = self.txn_id("vote", None);
        let path = format!(
            "_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            percent_encode(room_id),
            txn_id
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, Some(&virtual_user))
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("votePoll request failed: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to vote ({}): {}", status, error_body));
        }
        Ok(())
    }

    #[instrument(skip(self))]
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn post_location_impl(
        &self,
        room_id: &str,
        geo_uri: &str,
        description: Option<&str>,
        display_name: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        submission_id: Option<i64>,
    ) -> Result<String> {
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        self.ensure_joined(room_id, &virtual_user).await?;
        let guest_id = derive_guest_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow!("invalid author public key"))?;
        // Keep the display name in sync (best-effort), like text posts.
        if let Err(e) = self.ensure_display_name(&virtual_user, display_name).await {
            warn!("Failed to set display name for {}: {:#}", virtual_user, e);
        }
        let body = build_location_body(
            geo_uri,
            description,
            display_name,
            author_public_key,
            author_signature,
            author_challenge,
            &guest_id,
            submission_id,
        );
        let txn_id = self.txn_id("locate", submission_id);
        let path = format!(
            "_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            percent_encode(room_id),
            txn_id
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, Some(&virtual_user))
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("postLocation request failed: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to post location ({}): {}",
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
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn update_message_impl(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        display_name: &str,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        submission_id: Option<i64>,
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
            submission_id,
        );

        let txn_id = self.txn_id("update", submission_id);
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
                return Err(room_gone(
                    room_id,
                    format!("Failed to update message ({status}): {error_body}"),
                ));
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
    pub(super) async fn redact_message_impl(
        &self,
        room_id: &str,
        event_id: &str,
        submission_id: Option<i64>,
        proof: Option<&serde_json::Value>,
    ) -> Result<()> {
        // Redact as the sender user (has admin power level in the room).
        let txn_id = self.txn_id("delete", submission_id);
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
            if error_body.contains("M_FORBIDDEN") || error_body.contains("M_NOT_FOUND") {
                return Err(room_gone(
                    room_id,
                    format!("Failed to redact message ({status}): {error_body}"),
                ));
            }
            return Err(anyhow!(
                "Failed to redact message ({}): {}",
                status,
                error_body
            ));
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub(super) async fn event_exists_impl(&self, room_id: &str, event_id: &str) -> Result<bool> {
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
    pub(super) async fn get_room_events_impl(
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
}

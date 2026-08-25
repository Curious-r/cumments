//! Sending, editing, redacting and reading room messages.

use super::*;
use crate::wire::{
    build_edit_body, build_location_body, build_media_body, build_message_body,
    build_poll_vote_body, build_reaction_body, build_redaction_body, percent_encode,
};
use anyhow::{Result, anyhow};
use bytes::Bytes;
use cumments_core::{
    models::{CommentMedia, MatrixEvent, RoomEventPage, SiteId},
    submissions::fresh_transaction_id,
};
use serde::Deserialize;
use tracing::{instrument, warn};

#[derive(Deserialize)]
struct SendEventResponse {
    event_id: String,
}

#[derive(Deserialize)]
struct MessagesResponse {
    /// The pagination token for the next older page. Homeservers omit it on
    /// the final (or empty) page, so it must be optional.
    end: Option<String>,
    chunk: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct EventResponse {
    event_id: String,
    room_id: String,
    #[serde(rename = "type")]
    event_type: String,
    state_key: Option<String>,
    sender: Option<String>,
    origin_server_ts: i64,
    content: serde_json::Value,
    unsigned: Option<EventUnsigned>,
}

#[derive(Deserialize)]
struct EventUnsigned {
    redacted_because: Option<EventRedactedBecause>,
}

#[derive(Deserialize)]
struct EventRedactedBecause {
    event_id: String,
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

    /// Uploads media as the author's virtual user, using the same AppService
    /// identity seam as every other Cumments-initiated write.
    #[instrument(skip(self, bytes))]
    pub(super) async fn upload_media_impl(
        &self,
        bytes: Bytes,
        filename: &str,
        mimetype: &str,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<String> {
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        let resp = self
            .request(
                reqwest::Method::POST,
                "_matrix/media/v3/upload",
                Some(&virtual_user),
            )
            .query(&[("filename", filename)])
            .header("Content-Type", mimetype)
            .body(bytes)
            .send()
            .await
            .map_err(|e| anyhow!("media upload request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("media upload failed ({status}): {error_body}"));
        }
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow!("failed to parse media upload response: {e}"))?;
        data.get("content_uri")
            .and_then(|uri| uri.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("media upload response missing content_uri"))
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
        thread_root: Option<&str>,
        reply_to_body: Option<&str>,
        reply_to_sender: Option<&str>,
        submission_id: Option<i64>,
        txn_id: &str,
    ) -> Result<String> {
        // 1. Resolve virtual user via the store (includes site_id)
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;

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
                author_public_key,
                author_signature,
                author_challenge,
                submission_id,
            ),
            None => build_message_body(
                content,
                author_public_key,
                author_signature,
                author_challenge,
                submission_id,
                reply_to,
                thread_root,
                reply_to_body,
                reply_to_sender,
            ),
        };

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
    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
    pub(super) async fn react_message_impl(
        &self,
        room_id: &str,
        target_event_id: &str,
        key: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        txn_id: &str,
    ) -> Result<()> {
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        self.ensure_joined(room_id, &virtual_user).await?;
        let body = build_reaction_body(
            key,
            target_event_id,
            author_public_key,
            author_signature,
            author_challenge,
        );
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
    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
    pub(super) async fn vote_poll_impl(
        &self,
        room_id: &str,
        poll_event_id: &str,
        answer_id: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        txn_id: &str,
    ) -> Result<()> {
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        self.ensure_joined(room_id, &virtual_user).await?;
        let body = build_poll_vote_body(
            poll_event_id,
            answer_id,
            author_public_key,
            author_signature,
            author_challenge,
        );
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
    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
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
        reply_to: Option<&str>,
        thread_root: Option<&str>,
        txn_id: &str,
    ) -> Result<String> {
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;
        self.ensure_joined(room_id, &virtual_user).await?;
        // Keep the display name in sync (best-effort), like text posts.
        if let Err(e) = self.ensure_display_name(&virtual_user, display_name).await {
            warn!("Failed to set display name for {}: {:#}", virtual_user, e);
        }
        let body = build_location_body(
            geo_uri,
            description,
            author_public_key,
            author_signature,
            author_challenge,
            submission_id,
            reply_to,
            thread_root,
        );
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
    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
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
        txn_id: &str,
    ) -> Result<String> {
        // 1. Resolve virtual user (includes site_id)
        let virtual_user = self
            .resolve_virtual_user(author_public_key, site_id)
            .await?;

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
            author_public_key,
            author_signature,
            author_challenge,
            submission_id,
        );

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
        txn_id: &str,
    ) -> Result<String> {
        // Redact as the sender user (has admin power level in the room).
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

        let data: SendEventResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse redact response: {}", e))?;
        Ok(data.event_id)
    }

    #[instrument(skip(self))]
    pub(super) async fn get_event_impl(
        &self,
        room_id: &str,
        event_id: &str,
    ) -> Result<Option<MatrixEvent>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/event/{}",
            percent_encode(room_id),
            percent_encode(event_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to fetch event {}: {}", event_id, e))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Event fetch {} failed ({}): {}",
                event_id,
                status,
                error_body
            ));
        }

        let data: EventResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse event {}: {}", event_id, e))?;
        Ok(Some(MatrixEvent {
            event_id: data.event_id,
            room_id: data.room_id,
            event_type: data.event_type,
            state_key: data.state_key,
            sender: data.sender,
            origin_server_ts: data.origin_server_ts,
            content: data.content,
            redacted_by: data
                .unsigned
                .and_then(|unsigned| unsigned.redacted_because)
                .map(|because| because.event_id),
        }))
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
            next_token: data.end.clone(),
            has_more: data.end.is_some(),
        })
    }

    /// Sends a plain-text reply as the AS sender.
    pub(super) async fn send_bot_message_impl(&self, room_id: &str, body: &str) -> Result<String> {
        let txn_id = fresh_transaction_id("bot");
        let path = format!(
            "_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            percent_encode(room_id),
            percent_encode(&txn_id)
        );
        let resp = self
            .request(reqwest::Method::PUT, &path, None)
            .json(&serde_json::json!({ "msgtype": "m.text", "body": body }))
            .send()
            .await
            .map_err(|e| anyhow!("Bot message request failed: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Bot message to {room_id} failed ({status}): {error_body}"
            ));
        }
        let data: SendEventResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse send response: {}", e))?;
        Ok(data.event_id)
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

    #[tokio::test]
    async fn join_room_as_sender_accepts_claim_dm() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/rooms/%21dm%3Ahs/join"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!dm:hs" })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        driver
            .join_room("!dm:hs")
            .await
            .expect("join should succeed");
        server.verify().await;
    }

    #[tokio::test]
    async fn leave_room_as_sends_the_target_user() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/rooms/%21room%3Ahs/leave"))
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
            .leave_room_as("!room:hs", "@_cumments_my-blog_pubkey:example.com")
            .await
            .expect("leave should succeed");
        server.verify().await;
    }

    #[tokio::test]
    async fn joined_members_lists_member_mxids() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_matrix/client/v3/rooms/%21room%3Ahs/joined_members"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "joined": {
                    "@_cumments_bot:hs": {"display_name": "bot"},
                    "@alice:hs": {"display_name": "Alice"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let members = driver
            .get_joined_members("!room:hs")
            .await
            .expect("list members");
        assert_eq!(members.len(), 2);
        assert!(members.contains(&"@_cumments_bot:hs".to_string()));
        assert!(members.contains(&"@alice:hs".to_string()));
        server.verify().await;
    }

    #[tokio::test]
    async fn bot_message_sends_as_the_sender() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(body_partial_json(json!({
                "msgtype": "m.text",
                "body": "hello from bot",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$reply:hs" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let event_id = driver
            .send_bot_message("!room:hs", "hello from bot")
            .await
            .expect("send bot message");
        assert_eq!(event_id, "$reply:hs");
        server.verify().await;
    }

    #[tokio::test]
    async fn room_events_parse_final_empty_page_without_end() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("dir", "b"))
            .and(query_param("from", "4142337"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "start": "4142337",
                "chunk": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let page = driver
            .get_room_events_impl("!room:hs", Some("4142337"), 100)
            .await
            .expect("final empty page should parse without an end token");
        assert!(page.events.is_empty());
        assert!(!page.has_more);
        assert!(page.next_token.is_none());
        server.verify().await;
    }

    #[tokio::test]
    async fn upload_media_sends_as_the_authors_virtual_user() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_matrix/media/v3/upload"))
            .and(query_param(
                "user_id",
                "@_cumments_my-blog_pubkey:example.com",
            ))
            .and(query_param("filename", "cat.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "content_uri": "mxc://example.com/abc" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let url = driver
            .upload_media_impl(
                Bytes::from_static(b"image-bytes"),
                "cat.png",
                "image/png",
                "pubkey",
                &SiteId::from("my-blog"),
            )
            .await
            .expect("upload should succeed");
        assert_eq!(url, "mxc://example.com/abc");
        server.verify().await;
    }
}

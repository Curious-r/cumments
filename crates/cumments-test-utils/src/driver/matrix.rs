//! The single `MatrixDriver` implementation for [`TestDriver`].
//!
//! Rust allows only one impl block per (trait, type), so all 26 methods live
//! here together, grouped by the surface they exercise. Methods that record
//! calls are written out explicitly; the rest panic with `unimplemented!()`
//! so tests fail loudly instead of silently passing on untracked driver
//! behavior.

use super::TestDriver;
use cumments_core::{
    models::{CommentMedia, MatrixEvent, PageSlug, RoomEventPage, SiteId, VisitorProfile},
    ports::{MatrixDriver, MatrixProfileDriver, StateRedactionRepairer},
    profile::ProfileDriverError,
};

#[async_trait::async_trait]
impl MatrixDriver for TestDriver {
    // ── Room lifecycle and membership ────────────────────────────────

    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        page_slug: &PageSlug,
        _space_id: &str,
        candidate_room_id: Option<&str>,
    ) -> anyhow::Result<String> {
        if let Some(candidate) = candidate_room_id {
            return Ok(candidate.to_string());
        }
        let room_id = format!("!room-{}-{}:hs", site_id.as_str(), page_slug.as_str());
        Ok(room_id)
    }
    async fn create_site_space(&self, site_id: &SiteId) -> anyhow::Result<String> {
        let space_id = format!("!space-{}:hs", site_id.as_str());
        self.power_levels
            .lock()
            .await
            .entry(space_id.clone())
            .or_insert_with(|| {
                serde_json::json!({
                    "users": {},
                    "events": {
                        "m.room.power_levels": 100,
                        "m.room.tombstone": 150,
                    },
                    "state_default": 50,
                })
            });
        Ok(space_id)
    }
    async fn set_room_name(&self, _room_id: &str, _name: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn leave_room(&self, room_id: &str) -> anyhow::Result<()> {
        self.left.lock().await.push(room_id.to_string());
        Ok(())
    }
    async fn leave_room_as(&self, room_id: &str, user_id: &str) -> anyhow::Result<()> {
        self.left_as
            .lock()
            .await
            .push((room_id.to_string(), user_id.to_string()));
        Ok(())
    }
    async fn join_room(&self, room_id: &str) -> anyhow::Result<()> {
        if self.fail_join_rooms.lock().await.contains(room_id) {
            anyhow::bail!("injected join failure for {}", room_id);
        }
        self.joined.lock().await.push(room_id.to_string());
        Ok(())
    }
    async fn remove_room_alias(
        &self,
        _site_id: &SiteId,
        _page_slug: Option<&PageSlug>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_joined_rooms(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.joined_rooms.lock().await.clone())
    }
    async fn get_joined_members(&self, room_id: &str) -> anyhow::Result<Vec<String>> {
        self.joined_member_queries
            .lock()
            .await
            .push(room_id.to_string());
        Ok(self.joined_members.lock().await.clone())
    }
    fn sender_user_id(&self) -> Option<String> {
        Some("@_cumments_bot:hs".to_string())
    }
    async fn invite_user(&self, room_id: &str, user_id: &str) -> anyhow::Result<()> {
        self.invites
            .lock()
            .await
            .push((room_id.to_string(), user_id.to_string()));
        Ok(())
    }

    // ── Content writes and bot replies ───────────────────────────────

    async fn upload_media(
        &self,
        bytes: bytes::Bytes,
        filename: &str,
        mimetype: &str,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> anyhow::Result<String> {
        self.uploaded_media
            .lock()
            .await
            .push((mimetype.to_string(), filename.to_string()));
        Ok(format!(
            "mxc://hs/{}/{}-{}-{}",
            site_id.as_str(),
            author_public_key,
            filename,
            bytes.len()
        ))
    }
    async fn set_avatar_url(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        avatar_url: Option<&str>,
    ) -> anyhow::Result<()> {
        self.avatar_updates.lock().await.push((
            author_public_key.to_string(),
            site_id.as_str().to_string(),
            avatar_url.map(str::to_string),
        ));
        Ok(())
    }
    async fn get_profile(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> anyhow::Result<Option<VisitorProfile>> {
        if *self.fail_get_profile.lock().await {
            anyhow::bail!("simulated Matrix driver get_profile failure");
        }
        Ok(self
            .visitor_profiles
            .lock()
            .await
            .get(&(site_id.as_str().to_string(), author_public_key.to_string()))
            .cloned())
    }
    #[allow(clippy::too_many_arguments)]
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        media: Option<&CommentMedia>,
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
    ) -> anyhow::Result<String> {
        let mut failures = self.post_message_failures.lock().await;
        if !failures.is_empty() {
            return Err(failures.remove(0));
        }
        drop(failures);
        let mut messages = self.posted_messages.lock().await;
        let event_id = format!("$msg-event-{}", messages.len() + 1);
        messages.push(crate::driver::RecordedPostMessage {
            room_id: room_id.to_string(),
            content: content.to_string(),
            media: media.cloned(),
            author_public_key: author_public_key.to_string(),
            author_signature: author_signature.to_string(),
            author_challenge: author_challenge.to_string(),
            site_id: site_id.clone(),
            reply_to: reply_to.map(str::to_string),
            thread_root: thread_root.map(str::to_string),
            reply_to_body: reply_to_body.map(str::to_string),
            reply_to_sender: reply_to_sender.map(str::to_string),
            submission_id,
            txn_id: txn_id.to_string(),
        });
        Ok(event_id)
    }
    async fn react_message(
        &self,
        _room_id: &str,
        _target_event_id: &str,
        _key: &str,
        _site_id: &SiteId,
        _author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
        _txn_id: &str,
    ) -> anyhow::Result<()> {
        self.reactions.lock().await.push((
            _room_id.to_string(),
            _target_event_id.to_string(),
            _key.to_string(),
            _txn_id.to_string(),
        ));
        Ok(())
    }
    async fn post_poll_response(
        &self,
        request: cumments_core::ports::PollResponseRequest<'_>,
    ) -> anyhow::Result<()> {
        let mut fail_count = self.fail_poll_response_count.lock().await;
        if *fail_count > 0 {
            *fail_count -= 1;
            return Err(anyhow::anyhow!("Simulated Matrix transport failure"));
        }
        drop(fail_count);

        let mut responses = self.poll_responses.lock().await;
        if !responses
            .iter()
            .any(|r| r.room_id == request.room_id && r.txn_id == request.txn_id)
        {
            responses.push(crate::driver::RecordedPollResponse {
                room_id: request.room_id.to_string(),
                poll_event_id: request.poll_event_id.to_string(),
                option_ids: request.option_ids.to_vec(),
                operation_id: request.operation_id.to_string(),
                txn_id: request.txn_id.to_string(),
            });
        }
        drop(responses);

        let mut ambig_count = self.ambiguous_poll_response_count.lock().await;
        if *ambig_count > 0 {
            *ambig_count -= 1;
            return Err(anyhow::anyhow!(
                "Simulated ambiguous transport failure (response lost)"
            ));
        }

        Ok(())
    }

    async fn post_poll_end(
        &self,
        request: cumments_core::ports::PollEndRequest<'_>,
    ) -> anyhow::Result<()> {
        let mut fail_count = self.fail_poll_end_count.lock().await;
        if *fail_count > 0 {
            *fail_count -= 1;
            return Err(anyhow::anyhow!("Simulated Matrix transport failure"));
        }
        drop(fail_count);

        let mut ends = self.poll_ends.lock().await;
        if !ends
            .iter()
            .any(|r| r.room_id == request.room_id && r.txn_id == request.txn_id)
        {
            ends.push(crate::driver::RecordedPollEnd {
                room_id: request.room_id.to_string(),
                poll_event_id: request.poll_event_id.to_string(),
                operation_id: request.operation_id.to_string(),
                txn_id: request.txn_id.to_string(),
            });
        }
        drop(ends);

        let mut ambig_count = self.ambiguous_poll_end_count.lock().await;
        if *ambig_count > 0 {
            *ambig_count -= 1;
            return Err(anyhow::anyhow!(
                "Simulated ambiguous transport failure (response lost)"
            ));
        }

        Ok(())
    }

    async fn post_poll(
        &self,
        request: cumments_core::ports::PollStartRequest<'_>,
    ) -> anyhow::Result<String> {
        self.polls.lock().await.push(crate::driver::RecordedPoll {
            room_id: request.room_id.to_string(),
            question: request.question.to_string(),
            answers: request.answers.to_vec(),
            kind: request.kind,
            max_selections: request.max_selections,
            operation_id: request.operation_id.to_string(),
            submission_id: request.submission_id,
            txn_id: request.txn_id.to_string(),
        });
        Ok(format!("poll_event_{}", request.submission_id.unwrap_or(0)))
    }
    #[allow(clippy::too_many_arguments)]
    async fn post_location(
        &self,
        room_id: &str,
        geo_uri: &str,
        description: Option<&str>,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        submission_id: Option<i64>,
        reply_to: Option<&str>,
        thread_root: Option<&str>,
        txn_id: &str,
    ) -> anyhow::Result<String> {
        let mut locations = self.posted_locations.lock().await;
        let event_id = format!("$loc-event-{}", locations.len() + 1);
        locations.push(crate::driver::RecordedLocation {
            room_id: room_id.to_string(),
            geo_uri: geo_uri.to_string(),
            description: description.map(str::to_string),
            site_id: site_id.clone(),
            author_public_key: author_public_key.to_string(),
            author_signature: author_signature.to_string(),
            author_challenge: author_challenge.to_string(),
            submission_id,
            reply_to: reply_to.map(str::to_string),
            thread_root: thread_root.map(str::to_string),
            txn_id: txn_id.to_string(),
        });
        Ok(event_id)
    }
    #[allow(clippy::too_many_arguments)]
    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        submission_id: Option<i64>,
        txn_id: &str,
    ) -> anyhow::Result<String> {
        let mut failures = self.update_message_failures.lock().await;
        if !failures.is_empty() {
            return Err(failures.remove(0));
        }
        drop(failures);
        let mut updates = self.updated_messages.lock().await;
        let edit_event_id = format!("$edit-event-{}", updates.len() + 1);
        updates.push(crate::driver::RecordedUpdate {
            room_id: room_id.to_string(),
            event_id: event_id.to_string(),
            new_content: new_content.to_string(),
            author_public_key: author_public_key.to_string(),
            author_signature: author_signature.to_string(),
            author_challenge: author_challenge.to_string(),
            site_id: site_id.clone(),
            submission_id,
            txn_id: txn_id.to_string(),
        });
        Ok(edit_event_id)
    }
    #[allow(clippy::too_many_arguments)]
    async fn redact_message(
        &self,
        room_id: &str,
        event_id: &str,
        _submission_id: Option<i64>,
        _proof: Option<&serde_json::Value>,
        txn_id: &str,
    ) -> anyhow::Result<String> {
        let mut failures = self.redact_message_failures.lock().await;
        if !failures.is_empty() {
            return Err(failures.remove(0));
        }
        drop(failures);
        self.redactions.lock().await.push((
            room_id.to_string(),
            event_id.to_string(),
            txn_id.to_string(),
        ));
        Ok(format!("{}-redacted", event_id))
    }
    async fn send_bot_message(&self, room_id: &str, body: &str) -> anyhow::Result<String> {
        self.replies
            .lock()
            .await
            .push((room_id.to_string(), body.to_string()));
        Ok("$reply:hs".to_string())
    }

    // ── Reads and room state ─────────────────────────────────────────

    async fn get_room_events(
        &self,
        room_id: &str,
        _from: Option<&str>,
        _limit: u32,
    ) -> anyhow::Result<RoomEventPage> {
        Ok(RoomEventPage {
            events: self
                .room_events
                .lock()
                .await
                .get(room_id)
                .cloned()
                .unwrap_or_default(),
            next_token: None,
            has_more: false,
        })
    }
    async fn get_room_metadata(&self, room_id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(self.room_metadata.lock().await.get(room_id).cloned())
    }
    async fn get_room_canonical_alias(&self, _room_id: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    async fn event_exists(&self, _room_id: &str, _event_id: &str) -> anyhow::Result<bool> {
        unimplemented!("not used in this test")
    }
    async fn get_event(
        &self,
        _room_id: &str,
        _event_id: &str,
    ) -> anyhow::Result<Option<MatrixEvent>> {
        Ok(self
            .events
            .lock()
            .await
            .get(&(_room_id.to_string(), _event_id.to_string()))
            .cloned())
    }
    async fn get_room_power_levels(
        &self,
        room_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(self.power_levels.lock().await.get(room_id).cloned())
    }
    async fn set_room_power_levels(
        &self,
        room_id: &str,
        content: &serde_json::Value,
    ) -> anyhow::Result<()> {
        self.power_levels
            .lock()
            .await
            .insert(room_id.to_string(), content.clone());
        Ok(())
    }

    async fn get_room_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(self
            .room_state
            .lock()
            .await
            .get(&(
                room_id.to_string(),
                event_type.to_string(),
                state_key.to_string(),
            ))
            .cloned())
    }

    async fn set_room_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
        content: &serde_json::Value,
    ) -> anyhow::Result<String> {
        let key = (
            room_id.to_string(),
            event_type.to_string(),
            state_key.to_string(),
        );
        self.state_writes.lock().await.push(key.clone());
        let id = format!("$state-{}", self.state_writes.lock().await.len());
        self.room_state.lock().await.insert(key, content.clone());
        Ok(id)
    }

    async fn upgrade_room(&self, room_id: &str, new_version: &str) -> anyhow::Result<String> {
        let index = {
            let mut upgrades = self.upgrades.lock().await;
            upgrades.push((room_id.to_string(), new_version.to_string()));
            upgrades.len()
        };
        // Simulate the homeserver's idempotency: an existing tombstone wins,
        // otherwise the upgrade writes one for the new replacement room.
        let key = (
            room_id.to_string(),
            "m.room.tombstone".to_string(),
            String::new(),
        );
        if let Some(content) = self.room_state.lock().await.get(&key).cloned() {
            return Ok(content
                .get("replacement_room")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string());
        }
        let replacement = format!("!upgraded-{index}:hs");
        self.room_state
            .lock()
            .await
            .insert(key, serde_json::json!({ "replacement_room": replacement }));
        self.room_state.lock().await.insert(
            (
                replacement.clone(),
                "m.room.create".to_string(),
                String::new(),
            ),
            serde_json::json!({
                "room_version": new_version,
                "predecessor": { "room_id": room_id },
            }),
        );
        Ok(replacement)
    }

    async fn adopt_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        page_slug: Option<&PageSlug>,
        _require_space: bool,
    ) -> anyhow::Result<()> {
        self.adoptions.lock().await.push(room_id.to_string());
        let mut metadata = serde_json::json!({ "site_id": site_id.as_str() });
        if let Some(slug) = page_slug {
            metadata["page_slug"] = serde_json::json!(slug.as_str());
        }
        self.room_metadata
            .lock()
            .await
            .insert(room_id.to_string(), metadata);
        Ok(())
    }

    async fn link_room_to_space(&self, space_id: &str, room_id: &str) -> anyhow::Result<()> {
        self.space_links
            .lock()
            .await
            .push((space_id.to_string(), room_id.to_string()));
        Ok(())
    }
}

#[async_trait::async_trait]
impl StateRedactionRepairer for TestDriver {
    async fn repair_state_redaction(&self, _target_event_id: &str) -> anyhow::Result<()> {
        unimplemented!("not used in this test")
    }
}

#[async_trait::async_trait]
impl MatrixProfileDriver for TestDriver {
    async fn set_display_name(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        display_name: &str,
    ) -> Result<(), ProfileDriverError> {
        if let Some(err) = self.next_profile_error.lock().await.take() {
            return Err(err);
        }
        self.set_display_name_calls.lock().await.push((
            author_public_key.to_string(),
            site_id.clone(),
            display_name.to_string(),
        ));
        let mut profiles = self.visitor_profiles.lock().await;
        let entry = profiles
            .entry((site_id.as_str().to_string(), author_public_key.to_string()))
            .or_insert_with(|| VisitorProfile {
                display_name: None,
                avatar_url: None,
            });
        entry.display_name = Some(display_name.to_string());
        Ok(())
    }

    async fn clear_display_name(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<(), ProfileDriverError> {
        if let Some(err) = self.next_profile_error.lock().await.take() {
            return Err(err);
        }
        self.clear_display_name_calls
            .lock()
            .await
            .push((author_public_key.to_string(), site_id.clone()));
        let mut profiles = self.visitor_profiles.lock().await;
        if let Some(entry) =
            profiles.get_mut(&(site_id.as_str().to_string(), author_public_key.to_string()))
        {
            entry.display_name = None;
        }
        Ok(())
    }

    async fn set_avatar(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        avatar_url: &str,
    ) -> Result<(), ProfileDriverError> {
        if let Some(err) = self.next_profile_error.lock().await.take() {
            return Err(err);
        }
        self.set_avatar_calls.lock().await.push((
            author_public_key.to_string(),
            site_id.clone(),
            avatar_url.to_string(),
        ));
        let mut profiles = self.visitor_profiles.lock().await;
        let entry = profiles
            .entry((site_id.as_str().to_string(), author_public_key.to_string()))
            .or_insert_with(|| VisitorProfile {
                display_name: None,
                avatar_url: None,
            });
        entry.avatar_url = Some(avatar_url.to_string());
        Ok(())
    }

    async fn clear_avatar(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<(), ProfileDriverError> {
        if let Some(err) = self.next_profile_error.lock().await.take() {
            return Err(err);
        }
        self.clear_avatar_calls
            .lock()
            .await
            .push((author_public_key.to_string(), site_id.clone()));
        let mut profiles = self.visitor_profiles.lock().await;
        if let Some(entry) =
            profiles.get_mut(&(site_id.as_str().to_string(), author_public_key.to_string()))
        {
            entry.avatar_url = None;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl cumments_core::ports::HistoricalRoomStateResolver for TestDriver {
    async fn resolve_member_presentation(
        &self,
        room_id: &str,
        event_id: &str,
        sender_mxid: &str,
    ) -> anyhow::Result<Option<cumments_core::models::MemberPresentation>> {
        if *self.fail_historical_resolution.lock().await {
            return Err(anyhow::anyhow!("simulated historical resolution failure"));
        }
        if let Some(stub) = self.historical_stub.lock().await.as_ref() {
            return Ok(stub.clone());
        }
        Err(anyhow::anyhow!(
            "HistoricalRoomStateResolver is not supported by TestDriver for event {event_id} in {room_id} for {sender_mxid} without an explicit stub"
        ))
    }
}

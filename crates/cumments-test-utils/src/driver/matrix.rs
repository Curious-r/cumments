//! The single `MatrixDriver` implementation for [`TestDriver`].
//!
//! Rust allows only one impl block per (trait, type), so all 26 methods live
//! here together, grouped by the surface they exercise. Methods that record
//! calls are written out explicitly; the rest panic with `unimplemented!()`
//! so tests fail loudly instead of silently passing on untracked driver
//! behavior.

use super::TestDriver;
use cumments_core::{
    models::{CommentMedia, PageSlug, RoomEventPage, SiteId, VisitorProfile},
    ports::MatrixDriver,
};

#[async_trait::async_trait]
impl MatrixDriver for TestDriver {
    // ── Room lifecycle and membership ────────────────────────────────

    async fn ensure_comment_room(
        &self,
        _site_id: &SiteId,
        _page_slug: &PageSlug,
        _space_id: &str,
        _candidate_room_id: Option<&str>,
    ) -> anyhow::Result<String> {
        unimplemented!("not used in this test")
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

    async fn delete_media(&self, server: &str, media_id: &str) -> anyhow::Result<bool> {
        self.deleted
            .lock()
            .await
            .push((server.to_string(), media_id.to_string()));
        Ok(true)
    }
    async fn upload_media(
        &self,
        bytes: bytes::Bytes,
        filename: &str,
        _mimetype: &str,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> anyhow::Result<String> {
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
        _room_id: &str,
        _content: &str,
        _media: Option<&CommentMedia>,
        _display_name: &str,
        _author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
        _site_id: &SiteId,
        _reply_to: Option<&str>,
        _thread_root: Option<&str>,
        _reply_to_body: Option<&str>,
        _reply_to_sender: Option<&str>,
        _submission_id: Option<i64>,
        _txn_id: &str,
    ) -> anyhow::Result<String> {
        unimplemented!("not used in this test")
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
    async fn vote_poll(
        &self,
        _room_id: &str,
        _poll_event_id: &str,
        _answer_id: &str,
        _site_id: &SiteId,
        _author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
        _txn_id: &str,
    ) -> anyhow::Result<()> {
        self.poll_votes.lock().await.push((
            _room_id.to_string(),
            _poll_event_id.to_string(),
            _answer_id.to_string(),
            _txn_id.to_string(),
        ));
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    async fn post_location(
        &self,
        _room_id: &str,
        _geo_uri: &str,
        _description: Option<&str>,
        _display_name: &str,
        _site_id: &SiteId,
        _author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
        _submission_id: Option<i64>,
        _reply_to: Option<&str>,
        _thread_root: Option<&str>,
        _txn_id: &str,
    ) -> anyhow::Result<String> {
        unimplemented!("not used in this test")
    }
    #[allow(clippy::too_many_arguments)]
    async fn update_message(
        &self,
        _room_id: &str,
        _event_id: &str,
        _new_content: &str,
        _display_name: &str,
        _author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
        _site_id: &SiteId,
        _submission_id: Option<i64>,
        _txn_id: &str,
    ) -> anyhow::Result<String> {
        unimplemented!("not used in this test")
    }
    #[allow(clippy::too_many_arguments)]
    async fn redact_message(
        &self,
        _room_id: &str,
        _event_id: &str,
        _submission_id: Option<i64>,
        _proof: Option<&serde_json::Value>,
        _txn_id: &str,
    ) -> anyhow::Result<String> {
        unimplemented!("not used in this test")
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

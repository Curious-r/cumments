//! Shared test doubles for workspace tests.
//!
//! Kept in a dedicated crate so unit and integration tests across the
//! workspace exercise the same `MatrixDriver` fake instead of maintaining
//! near-identical copies per test module.

use cumments_core::{
    models::{CommentMedia, PostSlug, RoomEventPage, SiteId},
    ports::MatrixDriver,
};
use tokio::sync::Mutex;

/// In-memory [`MatrixDriver`] double that records the calls each test
/// asserts.
///
/// Methods outside the exercised surface panic with `unimplemented!()` so a
/// test that accidentally depends on untracked driver behavior fails loudly
/// instead of silently passing.
pub struct TestDriver {
    pub joined: Mutex<Vec<String>>,
    pub joined_members: Mutex<Vec<String>>,
    pub left: Mutex<Vec<String>>,
    pub left_as: Mutex<Vec<(String, String)>>,
    pub replies: Mutex<Vec<(String, String)>>,
    pub deleted: Mutex<Vec<(String, String)>>,
}

impl TestDriver {
    pub fn new() -> Self {
        Self {
            joined: Mutex::new(Vec::new()),
            joined_members: Mutex::new(Vec::new()),
            left: Mutex::new(Vec::new()),
            left_as: Mutex::new(Vec::new()),
            replies: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
        }
    }

    pub fn with_joined_members(members: Vec<String>) -> Self {
        Self {
            joined_members: Mutex::new(members),
            ..Self::new()
        }
    }
}

impl Default for TestDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl MatrixDriver for TestDriver {
    async fn ensure_comment_room(
        &self,
        _site_id: &SiteId,
        _post_slug: &PostSlug,
        _space_id: &str,
        _candidate_room_id: Option<&str>,
    ) -> anyhow::Result<String> {
        unimplemented!("not used in this test")
    }
    async fn create_site_space(&self, _site_id: &SiteId) -> anyhow::Result<String> {
        unimplemented!("not used in this test")
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
        _post_slug: Option<&PostSlug>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete_media(&self, server: &str, media_id: &str) -> anyhow::Result<bool> {
        self.deleted
            .lock()
            .await
            .push((server.to_string(), media_id.to_string()));
        Ok(true)
    }
    async fn upload_media(
        &self,
        _bytes: bytes::Bytes,
        _filename: &str,
        _mimetype: &str,
        _author_public_key: &str,
        _site_id: &SiteId,
    ) -> anyhow::Result<String> {
        unimplemented!("not used in this test")
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
    ) -> anyhow::Result<()> {
        unimplemented!("not used in this test")
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
    ) -> anyhow::Result<()> {
        unimplemented!("not used in this test")
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
    async fn get_room_events(
        &self,
        _room_id: &str,
        _from: Option<&str>,
        _limit: u32,
    ) -> anyhow::Result<RoomEventPage> {
        unimplemented!("not used in this test")
    }
    async fn get_joined_rooms(&self) -> anyhow::Result<Vec<String>> {
        unimplemented!("not used in this test")
    }
    async fn get_joined_members(&self, _room_id: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.joined_members.lock().await.clone())
    }
    async fn send_bot_message(&self, room_id: &str, body: &str) -> anyhow::Result<String> {
        self.replies
            .lock()
            .await
            .push((room_id.to_string(), body.to_string()));
        Ok("$reply:hs".to_string())
    }
    async fn get_room_metadata(&self, _room_id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        unimplemented!("not used in this test")
    }
    async fn get_room_canonical_alias(&self, _room_id: &str) -> anyhow::Result<Option<String>> {
        unimplemented!("not used in this test")
    }
    async fn event_exists(&self, _room_id: &str, _event_id: &str) -> anyhow::Result<bool> {
        unimplemented!("not used in this test")
    }
    fn sender_user_id(&self) -> Option<String> {
        Some("@_cumments_bot:hs".to_string())
    }
    async fn get_room_power_levels(
        &self,
        _room_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        unimplemented!("not used in this test")
    }
    async fn set_room_power_levels(
        &self,
        _room_id: &str,
        _content: &serde_json::Value,
    ) -> anyhow::Result<()> {
        unimplemented!("not used in this test")
    }
    async fn invite_user(&self, _room_id: &str, _user_id: &str) -> anyhow::Result<()> {
        unimplemented!("not used in this test")
    }
}

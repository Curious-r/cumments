use chrono::{Duration, Utc};
use cumments_core::{
    governance::{NewRoleClaim, OWNER_LEVEL},
    models::{CommentMedia, PostSlug, RoomEventPage, SiteId},
    ports::{MatrixDriver, RoleClaimStore},
};
use cumments_projector::{
    event_processor::{EventProcessor, EventProcessorDeps},
    parsed::ParsedRoomState,
};
use cumments_store::DbStore;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, broadcast};

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-claim-dm-invite-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

fn invite_event(room_id: &str, sender: &str) -> ParsedRoomState {
    ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$invite".to_string(),
        sender: sender.to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@_cumments_bot:hs".to_string(),
        origin_server_ts: 1,
        content: serde_json::json!({ "membership": "invite" }),
    }
}

fn processor(store: Arc<DbStore>, driver: Arc<TestDriver>) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
        role_claim_store: store.clone(),
        submission_store: store.clone(),
        driver: Some(driver),
        event_bus: tx,
        projection_notify: Arc::new(Notify::new()),
        server_name: Some("hs".to_string()),
    })
}

/// Minimal driver double: records joins and delegates everything else to
/// `unimplemented!()` because no other driver method is exercised here.
struct TestDriver {
    joined: Mutex<Vec<String>>,
}

impl TestDriver {
    fn new() -> Self {
        Self {
            joined: Mutex::new(Vec::new()),
        }
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
        unimplemented!("not used in this test")
    }
    async fn leave_room(&self, _room_id: &str) -> anyhow::Result<()> {
        unimplemented!("not used in this test")
    }
    async fn leave_room_as(&self, _room_id: &str, _user_id: &str) -> anyhow::Result<()> {
        unimplemented!("not used in this test")
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
        unimplemented!("not used in this test")
    }
    async fn delete_media(&self, _server: &str, _media_id: &str) -> anyhow::Result<bool> {
        unimplemented!("not used in this test")
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
        Ok(Vec::new())
    }
    async fn send_bot_message(&self, _room_id: &str, _body: &str) -> anyhow::Result<String> {
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

#[tokio::test]
async fn bot_joins_dm_only_when_inviter_has_a_pending_claim() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("with-claim"))
            .await
            .expect("connect db"),
    );
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "my-blog".to_string(),
            room_id: String::new(),
            user_id: "@owner:hs".to_string(),
            level: OWNER_LEVEL,
            token_hash: "hash".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        })
        .await
        .expect("upsert claim");

    let driver = Arc::new(TestDriver::new());
    processor(store.clone(), driver.clone())
        .process_room_state(invite_event("!dm:hs", "@owner:hs"))
        .await
        .expect("process invite");
    assert_eq!(*driver.joined.lock().await, vec!["!dm:hs"]);
    assert!(store.claim_dm_room_exists("!dm:hs").await.unwrap());

    let stranger_store = Arc::new(
        DbStore::connect(&test_db_url("without-claim"))
            .await
            .expect("connect db"),
    );
    let stranger_driver = Arc::new(TestDriver::new());
    processor(stranger_store, stranger_driver.clone())
        .process_room_state(invite_event("!other-dm:hs", "@stranger:hs"))
        .await
        .expect("process invite");
    assert!(stranger_driver.joined.lock().await.is_empty());
}

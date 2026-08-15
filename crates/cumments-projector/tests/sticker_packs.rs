use cumments_core::ports::{SiteStore, StickerPackStore};
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::ParsedRoomState;
use cumments_store::DbStore;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::broadcast;

mod common;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-sticker-projection-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn processor(store: Arc<DbStore>) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
        sticker_pack_store: store.clone(),
        role_claim_store: store.clone(),
        submission_store: store.clone(),
        audit_store: store.clone(),
        site_auth_store: store.clone(),
        site_auth_policy: common::test_policy(),
        site_service: Arc::new(cumments_core::site_service::SiteService::new(
            store.clone() as Arc<dyn cumments_core::ports::SiteStore>
        )),
        driver: None,
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: Arc::new(tokio::sync::Notify::new()),
        server_name: Some("hs".to_string()),
    })
}

async fn setup_site(store: &DbStore) {
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("ensure site");
}

fn pack_state(event_id: &str, ts: i64, content: serde_json::Value) -> ParsedRoomState {
    ParsedRoomState {
        room_id: "!space:hs".to_string(),
        event_id: event_id.to_string(),
        sender: "@owner:hs".to_string(),
        event_type: "m.room.image_pack".to_string(),
        state_key: "default".to_string(),
        origin_server_ts: ts,
        content,
    }
}

#[tokio::test]
async fn push_projects_pack_and_latest_state_wins() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("push"))
            .await
            .expect("connect db"),
    );
    setup_site(&store).await;
    let processor = processor(store.clone()).await;

    processor
        .process_room_state(pack_state(
            "$v1",
            100,
            json!({"images": {"cat": {"url": "mxc://hs/1"}}}),
        ))
        .await
        .expect("process v1");

    let packs = store.list_site_packs("my-blog").await.expect("list packs");
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0].event_id, "$v1");
    assert_eq!(packs[0].pack.content.images.len(), 1);

    processor
        .process_room_state(pack_state(
            "$v2",
            200,
            json!({"images": {"dog": {"url": "mxc://hs/2"}}}),
        ))
        .await
        .expect("process v2");

    let current = store
        .get_site_pack("my-blog", "default")
        .await
        .expect("get pack")
        .expect("pack exists");
    assert_eq!(current.event_id, "$v2");
    assert_eq!(current.pack.content.images[0].shortcode, "dog");
}

#[tokio::test]
async fn non_space_rooms_are_ignored() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("non-space"))
            .await
            .expect("connect db"),
    );
    let processor = processor(store.clone()).await;
    let mut event = pack_state(
        "$v1",
        100,
        json!({"images": {"cat": {"url": "mxc://hs/1"}}}),
    );
    event.room_id = "!other:hs".to_string();
    processor.process_room_state(event).await.expect("process");
    assert!(
        store
            .list_site_packs("my-blog")
            .await
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn usage_change_or_malformed_state_removes_the_projection() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("remove"))
            .await
            .expect("connect db"),
    );
    setup_site(&store).await;
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(pack_state(
            "$v1",
            100,
            json!({"images": {"cat": {"url": "mxc://hs/1"}}}),
        ))
        .await
        .expect("process v1");
    assert_eq!(
        store.list_site_packs("my-blog").await.expect("list").len(),
        1
    );

    // The pack no longer targets stickers: the current state replaces the
    // previous projection with nothing.
    processor
        .process_room_state(pack_state(
            "$v2",
            200,
            json!({"images": {"cat": {"url": "mxc://hs/1"}}, "pack": {"usage": ["emoticon"]}}),
        ))
        .await
        .expect("process emoticon pack");
    assert!(
        store
            .list_site_packs("my-blog")
            .await
            .expect("list")
            .is_empty()
    );

    // A pack whose images are all invalid still exists as an empty pack:
    // invalid entries are dropped, the current state remains authoritative.
    processor
        .process_room_state(pack_state(
            "$v3",
            300,
            json!({"images": {"bad!": {"url": "x"}}}),
        ))
        .await
        .expect("process malformed");
    let empty = store
        .get_site_pack("my-blog", "default")
        .await
        .expect("get pack")
        .expect("empty pack exists");
    assert!(empty.pack.content.images.is_empty());

    // A genuinely malformed replacement (non-object content) removes the
    // previous projection.
    processor
        .process_room_state(pack_state("$v4", 400, json!("not an object")))
        .await
        .expect("process non-object");
    assert!(
        store
            .list_site_packs("my-blog")
            .await
            .expect("list")
            .is_empty()
    );
}

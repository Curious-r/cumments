use cumments_core::ports::{SiteStore, StickerPackStore};
use cumments_core::sticker_packs::AddStickerInput;
use cumments_core::sticker_packs::{
    IMAGE_PACK_EVENT_TYPE, StickerPackUseCaseError, add_site_sticker, remove_site_sticker,
};
use cumments_projector::backfill::Backfiller;
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::{ParsedRoomRedaction, ParsedRoomState};
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
        projection_repair_store: store.clone(),
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

fn redaction(event_id: &str, ts: i64, redacts: &str) -> ParsedRoomRedaction {
    ParsedRoomRedaction {
        room_id: "!space:hs".to_string(),
        event_id: event_id.to_string(),
        sender: Some("@owner:hs".to_string()),
        origin_server_ts: ts,
        redacts: Some(redacts.to_string()),
        proof: None,
        submission_id: None,
        room_identity: None,
    }
}

fn room_create(event_id: &str, ts: i64) -> ParsedRoomState {
    ParsedRoomState {
        room_id: "!space:hs".to_string(),
        event_id: event_id.to_string(),
        sender: "@owner:hs".to_string(),
        event_type: "m.room.create".to_string(),
        state_key: String::new(),
        origin_server_ts: ts,
        content: json!({ "room_version": "12" }),
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

#[tokio::test]
async fn redacting_current_pack_removes_it_and_tombstones_replay() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("redact-current"))
            .await
            .expect("connect db"),
    );
    setup_site(&store).await;
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(room_create("$create", 50))
        .await
        .expect("process create");
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

    processor
        .process_room_redaction(redaction("$red", 200, "$v1"))
        .await
        .expect("process redaction");
    assert!(
        store
            .list_site_packs("my-blog")
            .await
            .expect("list")
            .is_empty()
    );

    // Re-delivering the original event (push retry / resumed backfill) must
    // not resurrect the redacted pack.
    processor
        .process_room_state(pack_state(
            "$v1",
            100,
            json!({"images": {"cat": {"url": "mxc://hs/1"}}}),
        ))
        .await
        .expect("re-deliver v1");
    assert!(
        store
            .list_site_packs("my-blog")
            .await
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn redacting_an_old_version_keeps_the_current_pack() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("redact-old"))
            .await
            .expect("connect db"),
    );
    setup_site(&store).await;
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(room_create("$create", 50))
        .await
        .expect("process create");
    processor
        .process_room_state(pack_state(
            "$v1",
            100,
            json!({"images": {"cat": {"url": "mxc://hs/1"}}}),
        ))
        .await
        .expect("process v1");
    processor
        .process_room_state(pack_state(
            "$v2",
            200,
            json!({"images": {"dog": {"url": "mxc://hs/2"}}}),
        ))
        .await
        .expect("process v2");

    processor
        .process_room_redaction(redaction("$red", 300, "$v1"))
        .await
        .expect("process old redaction");

    let current = store
        .get_site_pack("my-blog", "default")
        .await
        .expect("get pack")
        .expect("pack exists");
    assert_eq!(current.event_id, "$v2");
}

async fn backfill_processor(
    store: Arc<DbStore>,
    driver: Arc<common::TestDriver>,
) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
        sticker_pack_store: store.clone(),
        projection_repair_store: store.clone(),
        role_claim_store: store.clone(),
        submission_store: store.clone(),
        audit_store: store.clone(),
        site_auth_store: store.clone(),
        site_auth_policy: common::test_policy(),
        site_service: Arc::new(cumments_core::site_service::SiteService::new(
            store.clone() as Arc<dyn cumments_core::ports::SiteStore>
        )),
        driver: Some(driver),
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: Arc::new(tokio::sync::Notify::new()),
        server_name: Some("hs".to_string()),
    })
}

fn raw_state_event(
    event_id: &str,
    ts: i64,
    state_key: &str,
    content: serde_json::Value,
) -> serde_json::Value {
    json!({
        "type": "m.room.image_pack",
        "event_id": event_id,
        "room_id": "!space:hs",
        "sender": "@owner:hs",
        "origin_server_ts": ts,
        "state_key": state_key,
        "content": content,
    })
}

fn raw_redaction_event(event_id: &str, ts: i64, redacts: &str) -> serde_json::Value {
    json!({
        "type": "m.room.redaction",
        "event_id": event_id,
        "room_id": "!space:hs",
        "sender": "@owner:hs",
        "origin_server_ts": ts,
        "redacts": redacts,
        "content": {},
    })
}

#[tokio::test]
async fn backfill_replays_packs_and_redactions_in_order() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("backfill"))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(
        common::TestDriver::with_joined_rooms(vec!["!space:hs".to_string()])
            .with_room_metadata("!space:hs", json!({"site_id": "my-blog"}))
            .with_room_events(
                "!space:hs",
                vec![
                    raw_state_event(
                        "$v1",
                        100,
                        "default",
                        json!({"images": {"cat": {"url": "mxc://hs/1"}}}),
                    ),
                    raw_state_event(
                        "$v2",
                        200,
                        "default",
                        json!({"images": {"dog": {"url": "mxc://hs/2"}}}),
                    ),
                    raw_redaction_event("$red", 300, "$v2"),
                ],
            ),
    );
    let processor = backfill_processor(store.clone(), driver.clone()).await;

    let summary = Backfiller::new(
        driver.clone(),
        Arc::new(processor),
        store.clone(),
        store.clone(),
        store.clone(),
    )
    .run(10)
    .await
    .expect("backfill");

    assert_eq!(summary.rooms, 1);
    // Redaction of the current pack removes it; the previous version does
    // not become current again (spec: redacted state keeps its slot empty).
    assert!(
        store
            .list_site_packs("my-blog")
            .await
            .expect("list packs")
            .is_empty()
    );
}

#[tokio::test]
async fn backfill_restores_packs_after_db_reset() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("backfill-restore"))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(
        common::TestDriver::with_joined_rooms(vec!["!space:hs".to_string()])
            .with_room_metadata("!space:hs", json!({"site_id": "my-blog"}))
            .with_room_events(
                "!space:hs",
                vec![raw_state_event(
                    "$v1",
                    100,
                    "default",
                    json!({"images": {"cat": {"url": "mxc://hs/1"}}}),
                )],
            ),
    );
    let processor = backfill_processor(store.clone(), driver.clone()).await;

    Backfiller::new(
        driver.clone(),
        Arc::new(processor),
        store.clone(),
        store.clone(),
        store.clone(),
    )
    .run(10)
    .await
    .expect("backfill");

    let packs = store.list_site_packs("my-blog").await.expect("list packs");
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0].pack.state_key, "default");
    assert_eq!(packs[0].pack.content.images[0].shortcode, "cat");
}

#[tokio::test]
async fn use_cases_read_modify_write_pack_state() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("use-cases"))
            .await
            .expect("connect db"),
    );
    setup_site(&store).await;
    let driver = Arc::new(common::TestDriver::new());

    add_site_sticker(
        store.as_ref(),
        driver.as_ref(),
        AddStickerInput {
            site_id: "my-blog",
            pack_id: "default",
            shortcode: "cat",
            url: "mxc://hs/1",
            body: Some("a cat".to_string()),
            info: None,
        },
    )
    .await
    .expect("add cat");

    let state = || async {
        driver
            .room_state
            .lock()
            .await
            .get(&(
                "!space:hs".to_string(),
                IMAGE_PACK_EVENT_TYPE.to_string(),
                "default".to_string(),
            ))
            .cloned()
            .expect("pack state written")
    };
    let written = state().await;
    assert_eq!(written["pack"]["usage"][0], "sticker");
    assert_eq!(written["images"]["cat"]["url"], "mxc://hs/1");

    add_site_sticker(
        store.as_ref(),
        driver.as_ref(),
        AddStickerInput {
            site_id: "my-blog",
            pack_id: "default",
            shortcode: "dog",
            url: "mxc://hs/2",
            body: None,
            info: None,
        },
    )
    .await
    .expect("add dog");
    remove_site_sticker(store.as_ref(), driver.as_ref(), "my-blog", "default", "cat")
        .await
        .expect("remove cat");

    let written = state().await;
    assert!(written["images"].get("cat").is_none());
    assert!(written["images"].get("dog").is_some());
    assert_eq!(
        driver.state_writes.lock().await.len(),
        3,
        "every add/remove rewrites the full state"
    );
}

#[tokio::test]
async fn use_cases_validate_input_and_missing_packs() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("use-case-errors"))
            .await
            .expect("connect db"),
    );
    setup_site(&store).await;
    let driver = Arc::new(common::TestDriver::new());

    let missing = remove_site_sticker(store.as_ref(), driver.as_ref(), "my-blog", "default", "cat")
        .await
        .expect_err("missing pack must fail");
    assert!(matches!(missing, StickerPackUseCaseError::PackNotFound(_)));

    let invalid = add_site_sticker(
        store.as_ref(),
        driver.as_ref(),
        AddStickerInput {
            site_id: "my-blog",
            pack_id: "default",
            shortcode: "bad!",
            url: "mxc://hs/1",
            body: None,
            info: None,
        },
    )
    .await
    .expect_err("invalid shortcode must fail");
    assert!(matches!(invalid, StickerPackUseCaseError::Invalid(_)));
}

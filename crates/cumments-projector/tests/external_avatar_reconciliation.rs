//! Integration tests for Matrix avatar projection via the projector.
//!
//! Verifies the unified projection rule: every Matrix-derived avatar (native
//! Matrix user, Cumments virtual user, or remote observation) is projected as
//! its plain `mxc://` URI into the member presentation, never touching media
//! upload-provenance records.

use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::broadcast;

use cumments_core::models::{PageSlug, SiteId};
use cumments_core::ports::{MatrixDriver, RegistryStore, RoomStore, SiteStore};
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::ParsedRoomState;
use cumments_store::DbStore;
use serde_json::json;

mod common;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-ext-avatar-recon-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn create_processor_with_driver(
    store: Arc<DbStore>,
    driver: Option<Arc<dyn MatrixDriver>>,
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
            store.clone() as Arc<dyn SiteStore>
        )),
        driver,
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(Notify::new()),
        projection_notify: Arc::new(Notify::new()),
        server_name: Some("hs".to_string()),
        historical_state_resolver: None,
    })
}

fn create_processor(store: Arc<DbStore>) -> EventProcessor {
    create_processor_with_driver(store, None)
}

fn member_join(room_id: &str, user_id: &str, mxc: &str, ts: i64) -> ParsedRoomState {
    ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: format!("$join-{}-{ts}", user_id.trim_start_matches('@')),
        sender: user_id.to_string(),
        event_type: "m.room.member".to_string(),
        state_key: user_id.to_string(),
        origin_server_ts: ts,
        content: json!({
            "membership": "join",
            "displayname": user_id,
            "avatar_url": mxc,
        }),
    }
}

async fn setup_room(name: &str) -> (Arc<DbStore>, SiteId, &'static str) {
    let store = Arc::new(
        DbStore::connect(&test_db_url(name))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::from("my-blog");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room("!comments:hs", &site_id, &PageSlug::from("post-1"))
        .await
        .expect("register room");
    (store, site_id, "!comments:hs")
}

#[tokio::test]
async fn member_observation_retains_avatar_mxc() {
    let (store, _site_id, room_id) = setup_room("member_avatar").await;
    let mxc = "mxc://hs/unknown-avatar-alice-123";

    let processor = create_processor(store.clone());
    processor
        .process_room_state(member_join(room_id, "@alice:hs", mxc, 1000))
        .await
        .expect("process room state");

    let member = store
        .get_member(room_id, "@alice:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.avatar_url.as_deref(), Some(mxc));
}

#[tokio::test]
async fn native_and_cumments_users_follow_the_same_projection_path() {
    let (store, _site_id, room_id) = setup_room("unified_projection").await;

    let owned_mxc = "mxc://hs/cumments-uploaded-avatar";
    let plain_mxc = "mxc://hs/native-user-avatar";

    let processor = create_processor(store.clone());
    processor
        .process_room_state(member_join(room_id, "@owned:hs", owned_mxc, 1000))
        .await
        .expect("process owned user");
    processor
        .process_room_state(member_join(room_id, "@native:hs", plain_mxc, 2000))
        .await
        .expect("process native user");

    // Both projections retain their avatar MXC regardless of upload records.
    let owned = store
        .get_member(room_id, "@owned:hs")
        .await
        .unwrap()
        .unwrap();
    let native = store
        .get_member(room_id, "@native:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owned.avatar_url.as_deref(), Some(owned_mxc));
    assert_eq!(native.avatar_url.as_deref(), Some(plain_mxc));
}

#[tokio::test]
async fn member_avatar_projection_is_stable_and_read_only() {
    let (store, _site_id, room_id) = setup_room("stable_identity").await;
    let mxc = "mxc://hs/shared-identity-avatar";

    let driver = Arc::new(common::TestDriver::new());
    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    processor
        .process_room_state(member_join(room_id, "@author:hs", mxc, 1000))
        .await
        .unwrap();
    let member = store
        .get_member(room_id, "@author:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.avatar_url.as_deref(), Some(mxc));

    // Replaying the same event produces the same presentation.
    processor
        .process_room_state(member_join(room_id, "@author:hs", mxc, 2000))
        .await
        .unwrap();

    // Rebuilding the projection reconstructs the same avatar.
    store.delete_member(room_id, "@author:hs").await.unwrap();
    processor
        .process_room_state(member_join(room_id, "@author:hs", mxc, 3000))
        .await
        .unwrap();
    let rebuilt = store
        .get_member(room_id, "@author:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rebuilt.avatar_url.as_deref(), Some(mxc));

    // Projection is one-way: no Matrix profile write was attempted.
    assert!(driver.set_avatar_calls.lock().await.is_empty());
    assert!(driver.clear_avatar_calls.lock().await.is_empty());
}

#[tokio::test]
async fn member_without_avatar_projects_no_avatar() {
    let (store, _site_id, room_id) = setup_room("no_avatar").await;
    let processor = create_processor(store.clone());

    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-dave".to_string(),
            sender: "@dave:hs".to_string(),
            event_type: "m.room.member".to_string(),
            state_key: "@dave:hs".to_string(),
            origin_server_ts: 1000,
            content: json!({ "membership": "join", "displayname": "Dave" }),
        })
        .await
        .expect("process dave");

    let dave = store
        .get_member(room_id, "@dave:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dave.avatar_url, None);
}

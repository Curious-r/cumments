//! State redaction semantics: live push and backfill replay must converge on
//! the same read model (v11/v12 redaction algorithm).

use cumments_core::ports::MessageStore;
use cumments_core::ports::{GovernanceStore, RoomStore, SiteStore};
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::{ParsedRoomRedaction, ParsedRoomState};
use cumments_store::DbStore;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::broadcast;

mod common;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-state-redaction-{}-{}.db",
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

fn state(
    room_id: &str,
    event_id: &str,
    ts: i64,
    event_type: &str,
    state_key: &str,
    content: serde_json::Value,
) -> ParsedRoomState {
    ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: event_id.to_string(),
        sender: "@alice:hs".to_string(),
        event_type: event_type.to_string(),
        state_key: state_key.to_string(),
        origin_server_ts: ts,
        content,
    }
}

fn redaction(room_id: &str, event_id: &str, ts: i64, redacts: &str) -> ParsedRoomRedaction {
    ParsedRoomRedaction {
        room_id: room_id.to_string(),
        event_id: event_id.to_string(),
        sender: Some("@alice:hs".to_string()),
        origin_server_ts: ts,
        redacts: Some(redacts.to_string()),
        proof: None,
        submission_id: None,
        room_identity: None,
    }
}

#[tokio::test]
async fn redacting_current_room_name_removes_it_from_metadata() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("name"))
            .await
            .expect("connect db"),
    );
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(state(
            "!room:hs",
            "$create:hs",
            50,
            "m.room.create",
            "",
            json!({ "room_version": "12" }),
        ))
        .await
        .expect("save create");
    processor
        .process_room_state(state(
            "!room:hs",
            "$name:hs",
            100,
            "m.room.name",
            "",
            json!({ "name": "old" }),
        ))
        .await
        .expect("save name");

    processor
        .process_room_redaction(redaction("!room:hs", "$red:hs", 200, "$name:hs"))
        .await
        .expect("redact name");

    let metadata = store
        .get_room_metadata("!room:hs")
        .await
        .expect("metadata")
        .expect("metadata exists");
    assert_eq!(
        metadata.name, None,
        "redacted room name must be removed (state slot keeps empty content)"
    );
    let raw = store
        .get_state_event("$name:hs")
        .await
        .expect("get raw")
        .expect("raw exists");
    assert_eq!(raw.content_json, json!({}));
}

#[tokio::test]
async fn redacting_an_old_state_version_keeps_the_current_projection() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("old-version"))
            .await
            .expect("connect db"),
    );
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(state(
            "!room:hs",
            "$create:hs",
            50,
            "m.room.create",
            "",
            json!({ "room_version": "12" }),
        ))
        .await
        .expect("save create");
    processor
        .process_room_state(state(
            "!room:hs",
            "$v1:hs",
            100,
            "m.room.name",
            "",
            json!({ "name": "old" }),
        ))
        .await
        .expect("save v1");
    processor
        .process_room_state(state(
            "!room:hs",
            "$v2:hs",
            200,
            "m.room.name",
            "",
            json!({ "name": "new" }),
        ))
        .await
        .expect("save v2");

    processor
        .process_room_redaction(redaction("!room:hs", "$red:hs", 300, "$v1:hs"))
        .await
        .expect("redact old version");

    let metadata = store
        .get_room_metadata("!room:hs")
        .await
        .expect("metadata")
        .expect("metadata exists");
    assert_eq!(metadata.name.as_deref(), Some("new"));
    assert_eq!(
        store
            .get_state_event("$v1:hs")
            .await
            .expect("get v1")
            .expect("v1 exists")
            .content_json,
        json!({})
    );
}

#[tokio::test]
async fn redacting_power_levels_keeps_protected_keys_and_roles() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("power-levels"))
            .await
            .expect("connect db"),
    );
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("attach space");
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(state(
            "!space:hs",
            "$create:hs",
            50,
            "m.room.create",
            "",
            json!({ "room_version": "12" }),
        ))
        .await
        .expect("save create");
    processor
        .process_room_state(state(
            "!space:hs",
            "$pl:hs",
            100,
            "m.room.power_levels",
            "",
            json!({
                "users": { "@owner:hs": 100, "@co:hs": 75 },
                "events": { "m.room.power_levels": 100 },
                "state_default": 50,
                "notifications": { "room": 50 },
            }),
        ))
        .await
        .expect("save power levels");

    processor
        .process_room_redaction(redaction("!space:hs", "$red:hs", 200, "$pl:hs"))
        .await
        .expect("redact power levels");

    // users/events/state_default are protected by the redaction algorithm.
    let roles = store.list_site_roles("my-blog").await.expect("list roles");
    assert!(
        roles
            .iter()
            .any(|r| r.user_id == "@owner:hs" && r.level == 100)
    );
    assert!(roles.iter().any(|r| r.user_id == "@co:hs" && r.level == 75));
    let raw = store
        .get_state_event("$pl:hs")
        .await
        .expect("get raw")
        .expect("raw exists");
    assert_eq!(raw.content_json["users"]["@owner:hs"], 100);
    assert!(raw.content_json.get("notifications").is_none());
}

#[tokio::test]
async fn redacting_member_keeps_membership_and_drops_profile() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("member"))
            .await
            .expect("connect db"),
    );
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(state(
            "!room:hs",
            "$create:hs",
            50,
            "m.room.create",
            "",
            json!({ "room_version": "12" }),
        ))
        .await
        .expect("save create");
    processor
        .process_room_state(state(
            "!room:hs",
            "$member:hs",
            100,
            "m.room.member",
            "@alice:hs",
            json!({
                "membership": "join",
                "displayname": "Alice",
                "avatar_url": "mxc://hs/a",
            }),
        ))
        .await
        .expect("save member");

    processor
        .process_room_redaction(redaction("!room:hs", "$red:hs", 200, "$member:hs"))
        .await
        .expect("redact member");

    let member = store
        .get_member("!room:hs", "@alice:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.membership, "join");
    assert_eq!(member.display_name, None);
    assert_eq!(member.avatar_url, None);
}

#[tokio::test]
async fn leaving_member_keeps_the_last_known_profile() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("leave-profile"))
            .await
            .expect("connect db"),
    );
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(state(
            "!room:hs",
            "$join:hs",
            100,
            "m.room.member",
            "@alice:hs",
            json!({
                "membership": "join",
                "displayname": "Alice",
                "avatar_url": "mxc://hs/a",
            }),
        ))
        .await
        .expect("save join");
    processor
        .process_room_state(state(
            "!room:hs",
            "$leave:hs",
            200,
            "m.room.member",
            "@alice:hs",
            json!({ "membership": "leave" }),
        ))
        .await
        .expect("save leave");

    let member = store
        .get_member("!room:hs", "@alice:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.membership, "leave");
    assert_eq!(
        member.display_name.as_deref(),
        Some("Alice"),
        "leave events must not wipe the last known profile"
    );
    assert_eq!(member.avatar_url.as_deref(), Some("mxc://hs/a"));
}

#[tokio::test]
async fn unknown_room_versions_fail_closed_without_tombstoning() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("unknown-version"))
            .await
            .expect("connect db"),
    );
    let processor = processor(store.clone()).await;
    processor
        .process_room_state(state(
            "!room:hs",
            "$create:hs",
            50,
            "m.room.create",
            "",
            json!({ "room_version": "custom-experimental" }),
        ))
        .await
        .expect("save create");
    processor
        .process_room_state(state(
            "!room:hs",
            "$name:hs",
            100,
            "m.room.name",
            "",
            json!({ "name": "secret" }),
        ))
        .await
        .expect("save name");

    let result = processor
        .process_room_redaction(redaction("!room:hs", "$red:hs", 200, "$name:hs"))
        .await
        .expect_err("unknown room version must fail the event");
    assert!(result.to_string().contains("unknown room version"));

    let raw = store
        .get_state_event("$name:hs")
        .await
        .expect("get raw")
        .expect("state survives");
    assert_eq!(raw.content_json, json!({ "name": "secret" }));

    // The redaction itself must not be tombstoned: the AppService transaction
    // is left unacknowledged so the homeserver can retry after reconciliation.
    assert!(
        !store
            .has_backfill_tombstone("$red:hs", "!room:hs")
            .await
            .expect("check redaction tombstone")
    );
}

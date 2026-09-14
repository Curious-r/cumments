//! Integration test for external Matrix avatar reconciliation via the projector.
//!
//! Verifies that when an `m.room.member` state event containing an avatar MXC URI
//! is ingested by the projector for a registered room:
//! 1. The MXC is reconciled into `media_references` with `is_external = true`.
//! 2. Repeated ingestion is idempotent: the same MediaReference is reused without duplicates.
//! 3. `media_uploads` ownership table is untouched.
//! 4. Existing room member projection remains intact without premature schema changes.
//! 5. Mapping persists across processor restarts.

use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::broadcast;

use cumments_core::models::{PageSlug, SiteId};
use cumments_core::ports::{
    MediaReferenceStore, MessageStore, RegistryStore, RoomStore, SiteStore,
};
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

fn create_processor(store: Arc<DbStore>) -> EventProcessor {
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
        driver: None,
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(Notify::new()),
        projection_notify: Arc::new(Notify::new()),
        server_name: Some("hs".to_string()),
        media_reference_store: Some(store.clone()),
    })
}

#[tokio::test]
async fn external_avatar_reconciled_from_room_member_event() {
    let db_url = test_db_url("external_avatar_reconciled");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("my-blog");
    let page_slug = PageSlug::from("post-1");
    let room_id = "!comments:hs";

    // Setup site and registered room
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());

    let external_mxc = "mxc://hs/ext-avatar-alice-123";

    // 1. Process an m.room.member join event with an external avatar MXC
    let member_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-alice-join".to_string(),
        sender: "@alice:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@alice:hs".to_string(),
        origin_server_ts: 1000,
        content: json!({
            "membership": "join",
            "displayname": "Alice In Chains",
            "avatar_url": external_mxc,
        }),
    };

    processor
        .process_room_state(member_event)
        .await
        .expect("process room state");

    // 2. Verify media_references has the durable mapping marked is_external = true
    let media_ref = store
        .find_reference(&site_id, external_mxc)
        .await
        .expect("find reference")
        .expect("reference exists");

    assert!(
        media_ref.as_str().starts_with("cumments-media:"),
        "MediaReference must have canonical format: {}",
        media_ref.as_str()
    );

    let record = store
        .get_record(&site_id, &media_ref)
        .await
        .expect("get record")
        .expect("record exists");

    assert_eq!(record.site_id, site_id);
    assert_eq!(record.mxc_uri, external_mxc);
    assert!(
        record.is_external,
        "externally observed avatar must be recorded with is_external = true"
    );

    // 3. Verify media_uploads is completely untouched
    let unused_uploads = store
        .list_unused_media_before(chrono::Utc::now() + chrono::Duration::hours(1))
        .await
        .expect("list unused media");
    assert!(
        unused_uploads.is_empty(),
        "external avatar discovery must never create media_uploads records"
    );

    let is_owned = store
        .media_upload_owned_by(
            external_mxc,
            "@alice:hs",
            site_id.as_str(),
            page_slug.as_str(),
        )
        .await
        .expect("media upload owned");
    assert!(
        !is_owned,
        "external avatar discovery must not create media upload ownership"
    );

    // 4. Verify room_members projection is preserved
    let member = store
        .get_member(room_id, "@alice:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.display_name.as_deref(), Some("Alice In Chains"));
    assert_eq!(member.avatar_url.as_deref(), Some(external_mxc));
    assert_eq!(member.membership, "join");

    // 5. Idempotent repeated ingestion: same event replayed
    let replay_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-alice-replay".to_string(),
        sender: "@alice:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@alice:hs".to_string(),
        origin_server_ts: 1500,
        content: json!({
            "membership": "join",
            "displayname": "Alice In Chains (Updated)",
            "avatar_url": external_mxc,
        }),
    };

    processor
        .process_room_state(replay_event)
        .await
        .expect("process replay");

    let media_ref_after = store
        .find_reference(&site_id, external_mxc)
        .await
        .expect("find reference after replay")
        .expect("reference exists");

    assert_eq!(
        media_ref, media_ref_after,
        "repeated ingestion must return the exact same MediaReference"
    );

    // 6. Another member on the same site sharing the same external avatar reuses mapping
    let bob_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-bob-join".to_string(),
        sender: "@bob:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@bob:hs".to_string(),
        origin_server_ts: 2000,
        content: json!({
            "membership": "join",
            "displayname": "Bob",
            "avatar_url": external_mxc,
        }),
    };

    processor
        .process_room_state(bob_event)
        .await
        .expect("process bob join");

    let media_ref_bob = store
        .find_reference(&site_id, external_mxc)
        .await
        .expect("find bob reference")
        .expect("reference exists");

    assert_eq!(
        media_ref, media_ref_bob,
        "multiple members with the same MXC must share the same site-scoped MediaReference"
    );

    // 7. A member with a different external avatar gets a distinct MediaReference
    let charlie_mxc = "mxc://hs/ext-avatar-charlie-456";
    let charlie_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-charlie-join".to_string(),
        sender: "@charlie:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@charlie:hs".to_string(),
        origin_server_ts: 2500,
        content: json!({
            "membership": "join",
            "displayname": "Charlie",
            "avatar_url": charlie_mxc,
        }),
    };

    processor
        .process_room_state(charlie_event)
        .await
        .expect("process charlie join");

    let media_ref_charlie = store
        .find_reference(&site_id, charlie_mxc)
        .await
        .expect("find charlie reference")
        .expect("reference exists");

    assert_ne!(
        media_ref, media_ref_charlie,
        "different MXC URIs must produce distinct MediaReferences"
    );

    let record_charlie = store
        .get_record(&site_id, &media_ref_charlie)
        .await
        .expect("get charlie record")
        .expect("record exists");
    assert!(record_charlie.is_external);

    // 8. Event without avatar does not create media reference
    let dave_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-dave-join".to_string(),
        sender: "@dave:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@dave:hs".to_string(),
        origin_server_ts: 3000,
        content: json!({
            "membership": "join",
            "displayname": "Dave (No Avatar)",
        }),
    };

    processor
        .process_room_state(dave_event)
        .await
        .expect("process dave join");

    let dave_member = store
        .get_member(room_id, "@dave:hs")
        .await
        .expect("get dave")
        .expect("dave exists");
    assert_eq!(dave_member.avatar_url, None);

    // 9. Durable persistence across processor restart
    let processor_v2 = create_processor(store.clone());
    let ref_reloaded = store
        .find_reference(&site_id, external_mxc)
        .await
        .expect("reload ref")
        .expect("ref persists");
    assert_eq!(media_ref, ref_reloaded);

    // Replay on restarted processor produces the same reference
    processor_v2
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$evt-alice-v2".to_string(),
            sender: "@alice:hs".to_string(),
            event_type: "m.room.member".to_string(),
            state_key: "@alice:hs".to_string(),
            origin_server_ts: 3500,
            content: json!({
                "membership": "join",
                "displayname": "Alice Final",
                "avatar_url": external_mxc,
            }),
        })
        .await
        .expect("re-process after restart");

    let ref_after_restart = store
        .find_reference(&site_id, external_mxc)
        .await
        .expect("find after restart")
        .expect("ref persists");
    assert_eq!(media_ref, ref_after_restart);

    // Ensure media_uploads remains empty throughout
    let final_unused = store
        .list_unused_media_before(chrono::Utc::now() + chrono::Duration::hours(1))
        .await
        .expect("list unused media");
    assert!(final_unused.is_empty());
}

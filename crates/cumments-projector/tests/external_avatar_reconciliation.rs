//! Integration tests for Matrix avatar reconciliation and provenance via the projector.
//!
//! Verifies the provenance rules for MediaReference mapping:
//! 1. Cumments-owned media (pre-existing mapping or backed by authoritative `media_uploads`)
//!    is recorded with `is_external = false` and preserved across `m.room.member` ingestion.
//! 2. Explicit external discovery establishes `is_external = true`.
//! 3. Existing provenance is stable:
//!    - `existing false + later external observation -> remains false`
//!    - `existing true + later normal observation -> remains true`
//! 4. Unknown ordinary member observation without authoritative local upload indication
//!    does NOT blindly create an `is_external = true` mapping.
//! 5. Repeated ingestion and restart durability preserve identity without duplicates.
//! 6. External discovery never modifies `media_uploads`.

use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::broadcast;

use cumments_core::media_reference::MediaReferenceSource;
use cumments_core::models::{PageSlug, SiteId, VisitorProfile};
use cumments_core::ports::{
    MatrixDriver, MediaReferenceStore, MessageStore, RegistryStore, RoomStore, SiteStore,
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
        media_reference_store: Some(store.clone()),
        historical_state_resolver: None,
    })
}

fn create_processor(store: Arc<DbStore>) -> EventProcessor {
    create_processor_with_driver(store, None)
}

#[tokio::test]
async fn unknown_ordinary_member_observation_does_not_create_external_reference() {
    let db_url = test_db_url("unknown_member_no_speculative_external");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("my-blog");
    let page_slug = PageSlug::from("post-1");
    let room_id = "!comments:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());

    let unknown_mxc = "mxc://hs/unknown-avatar-alice-123";

    // 1. Process an ordinary m.room.member event with an unknown avatar MXC
    let member_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-alice-join".to_string(),
        sender: "@alice:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@alice:hs".to_string(),
        origin_server_ts: 1000,
        content: json!({
            "membership": "join",
            "displayname": "Alice",
            "avatar_url": unknown_mxc,
        }),
    };

    processor
        .process_room_state(member_event)
        .await
        .expect("process room state");

    // 2. Room member projection must succeed as normal
    let member = store
        .get_member(room_id, "@alice:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.display_name.as_deref(), Some("Alice"));
    assert_eq!(member.avatar_url.as_deref(), Some(unknown_mxc));

    // 3. Crucial requirement: unknown ordinary member observation MUST NOT
    // blindly classify an unknown mapping as external!
    let maybe_ref = store
        .find_reference(&site_id, unknown_mxc)
        .await
        .expect("find reference");
    assert!(
        maybe_ref.is_none(),
        "ordinary m.room.member observation must not fabricate an external media mapping"
    );

    // 4. media_uploads table remains empty
    let unused_uploads = store
        .list_media_upload_candidates_before(chrono::Utc::now() + chrono::Duration::hours(1))
        .await
        .expect("list unused media");
    assert!(unused_uploads.is_empty());
}

#[tokio::test]
async fn cumments_owned_mapping_preserves_provenance_through_member_ingestion() {
    let db_url = test_db_url("cumments_owned_provenance_preserved");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("my-blog");
    let page_slug = PageSlug::from("post-1");
    let room_id = "!comments:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());

    // Scenario A: Pre-existing Cumments-owned mapping in media_references
    let cumments_mxc = "mxc://hs/cumments-premapped-avatar";
    let pre_ref = store
        .get_or_create_reference(&site_id, cumments_mxc, MediaReferenceSource::Cumments)
        .await
        .expect("pre-create reference");

    let pre_record = store
        .get_record(&site_id, &pre_ref)
        .await
        .expect("get record")
        .expect("record exists");
    assert!(!pre_record.is_external);
    assert_eq!(pre_record.source(), MediaReferenceSource::Cumments);

    // Ingest room member event using cumments_mxc
    let alice_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-alice".to_string(),
        sender: "@alice:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@alice:hs".to_string(),
        origin_server_ts: 1000,
        content: json!({
            "membership": "join",
            "displayname": "Alice",
            "avatar_url": cumments_mxc,
        }),
    };
    processor
        .process_room_state(alice_event)
        .await
        .expect("process alice");

    // Provenance must remain is_external = false
    let post_record = store
        .get_record(&site_id, &pre_ref)
        .await
        .expect("get record")
        .expect("record exists");
    assert!(
        !post_record.is_external,
        "Cumments-created media must remain is_external = false after member ingestion"
    );
    assert_eq!(post_record.source(), MediaReferenceSource::Cumments);

    // Scenario B: Media object present in media_uploads authoritative table
    let upload_mxc = "mxc://hs/cumments-upload-bob-avatar";
    store
        .record_media_upload(
            upload_mxc,
            "bob-key",
            site_id.as_str(),
            Some(page_slug.as_str()),
        )
        .await
        .expect("record upload");

    // Before member ingestion, no mapping exists yet
    assert!(
        store
            .find_reference(&site_id, upload_mxc)
            .await
            .unwrap()
            .is_none()
    );

    // Ingest room member event for Bob using upload_mxc
    let bob_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-bob".to_string(),
        sender: "@bob:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@bob:hs".to_string(),
        origin_server_ts: 1500,
        content: json!({
            "membership": "join",
            "displayname": "Bob",
            "avatar_url": upload_mxc,
        }),
    };
    processor
        .process_room_state(bob_event)
        .await
        .expect("process bob");

    // Mapping was created with Cumments provenance because media_uploads authoritative fact was present
    let bob_ref = store
        .find_reference(&site_id, upload_mxc)
        .await
        .expect("find bob ref")
        .expect("bob ref exists");
    let bob_record = store
        .get_record(&site_id, &bob_ref)
        .await
        .expect("get bob record")
        .expect("bob record exists");
    assert!(
        !bob_record.is_external,
        "authoritatively known upload must be created as is_external = false"
    );
    assert_eq!(bob_record.source(), MediaReferenceSource::Cumments);
}

#[tokio::test]
async fn explicit_external_discovery_creates_external_mapping() {
    let db_url = test_db_url("explicit_external_discovery");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("my-blog");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let driver = Arc::new(common::TestDriver::new());
    let author_pubkey = "author-ext-discovery-1";
    let ext_mxc = "mxc://hs/explicit-external-avatar-999";

    // Simulate an authoritative Matrix profile reading (external writer set avatar on global profile)
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_pubkey.to_string()),
        VisitorProfile {
            display_name: Some("External Author".to_string()),
            avatar_url: Some(ext_mxc.to_string()),
        },
    );

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // 1. Authoritative external Matrix profile observation feeds into actual reconciliation workflow
    let media_ref = processor
        .reconcile_visitor_profile(&site_id, author_pubkey)
        .await
        .expect("reconcile visitor profile")
        .expect("avatar reference returned");

    assert!(media_ref.as_str().starts_with("cumments-media:"));

    let record = store
        .get_record(&site_id, &media_ref)
        .await
        .expect("get record")
        .expect("record exists");

    assert_eq!(record.site_id, site_id);
    assert_eq!(record.mxc_uri, ext_mxc);
    assert!(
        record.is_external,
        "authoritative external Matrix profile observation must record is_external = true"
    );
    assert_eq!(record.source(), MediaReferenceSource::External);

    // 2. Repeated external profile observation reuses the exact same reference
    let media_ref_repeated = processor
        .reconcile_visitor_profile(&site_id, author_pubkey)
        .await
        .expect("repeat reconcile")
        .expect("same reference");
    assert_eq!(media_ref, media_ref_repeated);

    // 3. Explicit external profile avatar reconciliation entrypoint also creates/reuses external mapping
    let direct_ext_mxc = "mxc://hs/explicit-direct-external-avatar-888";
    let direct_ref = processor
        .reconcile_external_profile_avatar(&site_id, direct_ext_mxc)
        .await
        .expect("reconcile direct external");
    let direct_rec = store
        .get_record(&site_id, &direct_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(direct_rec.is_external);
    assert_eq!(direct_rec.source(), MediaReferenceSource::External);

    // 4. Case A: Cumments-owned media observed via profile read is NOT external
    let cumments_mxc = "mxc://hs/cumments-owned-profile-avatar";
    store
        .record_media_upload(
            cumments_mxc,
            "cumments-author-key",
            site_id.as_str(),
            Some("post-1"),
        )
        .await
        .expect("record upload");
    driver.visitor_profiles.lock().await.insert(
        (
            site_id.as_str().to_string(),
            "cumments-author-key".to_string(),
        ),
        VisitorProfile {
            display_name: Some("Cumments User".to_string()),
            avatar_url: Some(cumments_mxc.to_string()),
        },
    );
    let cumments_ref = processor
        .reconcile_visitor_profile(&site_id, "cumments-author-key")
        .await
        .expect("reconcile cumments user profile")
        .expect("cumments avatar ref");
    let cumments_rec = store
        .get_record(&site_id, &cumments_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !cumments_rec.is_external,
        "profile avatar with authoritative local upload record must be is_external = false"
    );
    assert_eq!(cumments_rec.source(), MediaReferenceSource::Cumments);

    // 5. Profile with no avatar returns None without error
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), "no-avatar-user".to_string()),
        VisitorProfile {
            display_name: Some("No Avatar".to_string()),
            avatar_url: None,
        },
    );
    let no_avatar_ref = processor
        .reconcile_visitor_profile(&site_id, "no-avatar-user")
        .await
        .expect("reconcile no avatar profile");
    assert!(no_avatar_ref.is_none());

    // 6. Ownership table must be completely untouched by external reconciliation
    let is_owned = store
        .media_upload_owned_by(ext_mxc, "@someone:hs", site_id.as_str(), "page")
        .await
        .expect("check upload owned");
    assert!(
        !is_owned,
        "external discovery must never touch media_uploads"
    );
    let is_direct_owned = store
        .media_upload_owned_by(direct_ext_mxc, "@someone:hs", site_id.as_str(), "page")
        .await
        .expect("check upload owned");
    assert!(
        !is_direct_owned,
        "direct external discovery must never touch media_uploads"
    );
}

#[tokio::test]
async fn existing_provenance_stability_across_mixed_observations() {
    let db_url = test_db_url("provenance_stability");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("my-blog");
    let page_slug = PageSlug::from("post-1");
    let room_id = "!comments:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let driver = Arc::new(common::TestDriver::new());
    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));
    let reconciler = processor.avatar_reconciler().unwrap();

    // 1. Existing false + later external observation -> remains false
    let cumments_mxc = "mxc://hs/cumments-stable-avatar";
    let cumments_ref = store
        .get_or_create_reference(&site_id, cumments_mxc, MediaReferenceSource::Cumments)
        .await
        .expect("create cumments ref");

    // Later external discovery of same MXC returns same ref and does NOT flip is_external
    let ext_discovered_ref = reconciler
        .reconcile_external_avatar(&site_id, cumments_mxc)
        .await
        .expect("reconcile existing cumments ref");
    assert_eq!(cumments_ref, ext_discovered_ref);

    let rec1 = store
        .get_record(&site_id, &cumments_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !rec1.is_external,
        "existing is_external = false must not be flipped by subsequent external observation"
    );
    assert_eq!(rec1.source(), MediaReferenceSource::Cumments);

    // Later external profile read of same MXC also returns same ref and does NOT flip is_external
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), "alice-key".to_string()),
        VisitorProfile {
            display_name: Some("Alice".to_string()),
            avatar_url: Some(cumments_mxc.to_string()),
        },
    );
    let profile_observed_ref = processor
        .reconcile_visitor_profile(&site_id, "alice-key")
        .await
        .expect("reconcile profile")
        .expect("ref returned");
    assert_eq!(cumments_ref, profile_observed_ref);

    let rec1_after_profile = store
        .get_record(&site_id, &cumments_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(!rec1_after_profile.is_external);
    assert_eq!(rec1_after_profile.source(), MediaReferenceSource::Cumments);

    // Subsequent room member ingestion also preserves is_external = false
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$evt-alice-1".to_string(),
            sender: "@alice:hs".to_string(),
            event_type: "m.room.member".to_string(),
            state_key: "@alice:hs".to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Alice",
                "avatar_url": cumments_mxc,
            }),
        })
        .await
        .expect("process alice");

    let rec1_after_member = store
        .get_record(&site_id, &cumments_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(!rec1_after_member.is_external);
    assert_eq!(rec1_after_member.source(), MediaReferenceSource::Cumments);

    // 2. Existing true + later normal observation -> remains true
    let ext_mxc = "mxc://hs/external-stable-avatar";
    let ext_ref = reconciler
        .reconcile_external_avatar(&site_id, ext_mxc)
        .await
        .expect("reconcile external avatar");

    let rec2 = store.get_record(&site_id, &ext_ref).await.unwrap().unwrap();
    assert!(rec2.is_external);
    assert_eq!(rec2.source(), MediaReferenceSource::External);

    // Later profile observation of the same MXC preserves is_external = true
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), "bob-key".to_string()),
        VisitorProfile {
            display_name: Some("Bob".to_string()),
            avatar_url: Some(ext_mxc.to_string()),
        },
    );
    let ext_profile_ref = processor
        .reconcile_visitor_profile(&site_id, "bob-key")
        .await
        .expect("reconcile profile")
        .expect("ref returned");
    assert_eq!(ext_ref, ext_profile_ref);

    let rec2_after_profile = store.get_record(&site_id, &ext_ref).await.unwrap().unwrap();
    assert!(rec2_after_profile.is_external);
    assert_eq!(rec2_after_profile.source(), MediaReferenceSource::External);

    // Later normal m.room.member observation of the same MXC
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$evt-bob-1".to_string(),
            sender: "@bob:hs".to_string(),
            event_type: "m.room.member".to_string(),
            state_key: "@bob:hs".to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "Bob",
                "avatar_url": ext_mxc,
            }),
        })
        .await
        .expect("process bob");

    let rec2_after_member = store.get_record(&site_id, &ext_ref).await.unwrap().unwrap();
    assert!(
        rec2_after_member.is_external,
        "existing is_external = true must remain true after room member ingestion"
    );
    assert_eq!(rec2_after_member.source(), MediaReferenceSource::External);
    assert_eq!(
        store.find_reference(&site_id, ext_mxc).await.unwrap(),
        Some(ext_ref)
    );
}

#[tokio::test]
async fn repeated_ingestion_and_restart_durability() {
    let db_url = test_db_url("repeated_ingestion_durability");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("my-blog");
    let page_slug = PageSlug::from("post-1");
    let room_id = "!comments:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());
    let reconciler = processor.avatar_reconciler().unwrap();

    let mxc_uri = "mxc://hs/shared-avatar-durable";
    let media_ref = reconciler
        .reconcile_external_avatar(&site_id, mxc_uri)
        .await
        .expect("reconcile external avatar");

    // 1. Process member event for Alice with this avatar
    let alice_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-alice-1".to_string(),
        sender: "@alice:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@alice:hs".to_string(),
        origin_server_ts: 1000,
        content: json!({
            "membership": "join",
            "displayname": "Alice",
            "avatar_url": mxc_uri,
        }),
    };
    processor
        .process_room_state(alice_event)
        .await
        .expect("process alice 1");

    // 2. Replay same event: idempotent, no error, same reference
    let alice_replay = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-alice-1".to_string(),
        sender: "@alice:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@alice:hs".to_string(),
        origin_server_ts: 1000,
        content: json!({
            "membership": "join",
            "displayname": "Alice",
            "avatar_url": mxc_uri,
        }),
    };
    processor
        .process_room_state(alice_replay)
        .await
        .expect("process alice replay");
    assert_eq!(
        store.find_reference(&site_id, mxc_uri).await.unwrap(),
        Some(media_ref.clone())
    );

    // 3. Another member on the same site sharing the same avatar reuses mapping
    let bob_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-bob-1".to_string(),
        sender: "@bob:hs".to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@bob:hs".to_string(),
        origin_server_ts: 2000,
        content: json!({
            "membership": "join",
            "displayname": "Bob",
            "avatar_url": mxc_uri,
        }),
    };
    processor
        .process_room_state(bob_event)
        .await
        .expect("process bob");
    assert_eq!(
        store.find_reference(&site_id, mxc_uri).await.unwrap(),
        Some(media_ref.clone())
    );

    // 4. Member without avatar does not create media references
    let dave_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$evt-dave-1".to_string(),
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
        .expect("process dave");

    let dave_member = store
        .get_member(room_id, "@dave:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dave_member.avatar_url, None);

    // 5. Restart durability: new processor instance on same DB
    let processor_v2 = create_processor(store.clone());
    let reloaded_ref = store
        .find_reference(&site_id, mxc_uri)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(media_ref, reloaded_ref);

    // Ingestion on restarted processor produces the same reference
    processor_v2
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$evt-alice-v2".to_string(),
            sender: "@alice:hs".to_string(),
            event_type: "m.room.member".to_string(),
            state_key: "@alice:hs".to_string(),
            origin_server_ts: 4000,
            content: json!({
                "membership": "join",
                "displayname": "Alice Updated",
                "avatar_url": mxc_uri,
            }),
        })
        .await
        .expect("process on v2");

    assert_eq!(
        store.find_reference(&site_id, mxc_uri).await.unwrap(),
        Some(media_ref.clone())
    );

    // Profile observation on restarted processor also yields the same reference
    let driver_v2 = Arc::new(common::TestDriver::new());
    driver_v2.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), "alice-author-key".to_string()),
        VisitorProfile {
            display_name: Some("Alice".to_string()),
            avatar_url: Some(mxc_uri.to_string()),
        },
    );
    let processor_v2_with_driver = create_processor_with_driver(store.clone(), Some(driver_v2));
    let profile_reloaded_ref = processor_v2_with_driver
        .reconcile_visitor_profile(&site_id, "alice-author-key")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(media_ref, profile_reloaded_ref);

    // Ensure media_uploads remains empty throughout
    let final_unused = store
        .list_media_upload_candidates_before(chrono::Utc::now() + chrono::Duration::hours(1))
        .await
        .unwrap();
    assert!(final_unused.is_empty());
}

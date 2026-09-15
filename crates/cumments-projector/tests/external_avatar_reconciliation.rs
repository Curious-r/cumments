//! Integration tests for Matrix avatar projection via the projector.
//!
//! Verifies the unified projection rule: every Matrix-derived avatar (native
//! Matrix user, Cumments virtual user, or remote observation) derives the same
//! deterministic MediaReference from `(site_id, mxc_uri)`, materializes a
//! resolvable lookup mapping, and never touches upload ownership bookkeeping.

use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::broadcast;

use cumments_core::media_reference::MediaReference;
use cumments_core::models::{PageSlug, SiteId, VisitorProfile};
use cumments_core::ports::{
    MatrixDriver, MediaReferenceResolver, MediaReferenceStore, MessageStore, RegistryStore,
    RoomStore, SiteStore,
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
async fn ordinary_member_observation_materializes_a_resolvable_mapping() {
    let (store, site_id, room_id) = setup_room("member_mapping").await;
    let mxc = "mxc://hs/unknown-avatar-alice-123";

    // A missing mapping before projection is normal.
    assert!(
        store
            .find_reference(&site_id, mxc)
            .await
            .expect("find reference")
            .is_none()
    );

    let processor = create_processor(store.clone());
    processor
        .process_room_state(member_join(room_id, "@alice:hs", mxc, 1000))
        .await
        .expect("process room state");

    let expected = MediaReference::from_media(&site_id, mxc);
    let member = store
        .get_member(room_id, "@alice:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.avatar_url.as_deref(), Some(mxc));
    assert_eq!(member.media_reference, Some(expected.clone()));

    // The persisted reference stays resolvable through the lookup mapping.
    assert_eq!(
        store.find_reference(&site_id, mxc).await.unwrap(),
        Some(expected.clone())
    );
    assert_eq!(
        store
            .resolve_mxc(&site_id, &expected)
            .await
            .unwrap()
            .as_deref(),
        Some(mxc)
    );

    // Projection never creates upload ownership evidence.
    let unused_uploads = store
        .list_media_upload_candidates_before(chrono::Utc::now() + chrono::Duration::hours(1))
        .await
        .unwrap();
    assert!(unused_uploads.is_empty());
}

#[tokio::test]
async fn native_and_cumments_users_follow_the_same_projection_path() {
    let (store, site_id, room_id) = setup_room("unified_projection").await;

    // A Cumments-owned upload record exists for one avatar only.
    let owned_mxc = "mxc://hs/cumments-uploaded-avatar";
    store
        .record_media_upload(owned_mxc, "owned-key", site_id.as_str(), Some("post-1"))
        .await
        .expect("record upload");
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

    // Both projections derive the deterministic identity, regardless of uploads.
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
    assert_eq!(
        owned.media_reference,
        Some(MediaReference::from_media(&site_id, owned_mxc))
    );
    assert_eq!(
        native.media_reference,
        Some(MediaReference::from_media(&site_id, plain_mxc))
    );

    // Ownership evidence is unchanged: only the uploaded avatar is owned.
    assert!(
        store
            .has_media_upload_for_site(site_id.as_str(), owned_mxc)
            .await
            .unwrap()
    );
    assert!(
        !store
            .has_media_upload_for_site(site_id.as_str(), plain_mxc)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn global_profile_projection_derives_reference_without_mapping() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("profile_projection"))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::from("my-blog");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let author = "author-projection-1";
    let mxc = "mxc://hs/global-profile-avatar";

    let driver = Arc::new(common::TestDriver::new());
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author.to_string()),
        VisitorProfile {
            display_name: Some("Projected".to_string()),
            avatar_url: Some(mxc.to_string()),
        },
    );

    // No lookup mapping exists before the profile is projected.
    assert!(
        store
            .find_reference(&site_id, mxc)
            .await
            .expect("find reference")
            .is_none()
    );

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));
    let reference = processor
        .reconcile_visitor_profile(&site_id, author)
        .await
        .expect("reconcile profile")
        .expect("avatar reference");

    // Identity comes from the deterministic derivation, not a stored row.
    assert_eq!(reference, MediaReference::from_media(&site_id, mxc));
    assert_eq!(
        store
            .resolve_mxc(&site_id, &reference)
            .await
            .unwrap()
            .as_deref(),
        Some(mxc)
    );

    // Projection stays one-way: no Matrix profile write was attempted.
    assert!(driver.set_avatar_calls.lock().await.is_empty());
    assert!(driver.clear_avatar_calls.lock().await.is_empty());
}

#[tokio::test]
async fn identity_is_stable_across_observation_paths() {
    let (store, site_id, room_id) = setup_room("stable_identity").await;
    let mxc = "mxc://hs/shared-identity-avatar";
    let expected = MediaReference::from_media(&site_id, mxc);

    let driver = Arc::new(common::TestDriver::new());
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), "author-1".to_string()),
        VisitorProfile {
            display_name: Some("Author".to_string()),
            avatar_url: Some(mxc.to_string()),
        },
    );
    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // Global profile projection followed by room member ingestion: same identity.
    let profile_ref = processor
        .reconcile_visitor_profile(&site_id, "author-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(profile_ref, expected);

    processor
        .process_room_state(member_join(room_id, "@author:hs", mxc, 1000))
        .await
        .unwrap();
    let member = store
        .get_member(room_id, "@author:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.media_reference, Some(expected.clone()));

    // Replaying the same event does not create a different identity.
    processor
        .process_room_state(member_join(room_id, "@author:hs", mxc, 2000))
        .await
        .unwrap();
    assert_eq!(
        store.find_reference(&site_id, mxc).await.unwrap(),
        Some(expected.clone())
    );

    // Deleting the lookup mapping and re-projecting recreates the same
    // deterministic reference and re-materializes the mapping.
    use cumments_store::sea_orm::ConnectionTrait;

    store.delete_member(room_id, "@author:hs").await.unwrap();
    store
        .connection()
        .execute_unprepared(&format!(
            "DELETE FROM media_references WHERE site_id = '{}' AND mxc_uri = '{}'",
            site_id.as_str(),
            mxc
        ))
        .await
        .expect("delete mapping");
    assert!(store.find_reference(&site_id, mxc).await.unwrap().is_none());

    processor
        .process_room_state(member_join(room_id, "@author:hs", mxc, 3000))
        .await
        .unwrap();
    let rebuilt = store
        .get_member(room_id, "@author:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rebuilt.media_reference, Some(expected.clone()));
    assert_eq!(
        store.find_reference(&site_id, mxc).await.unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn member_without_avatar_creates_no_mapping() {
    let (store, site_id, room_id) = setup_room("no_avatar").await;
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
    assert_eq!(dave.media_reference, None);
    assert!(
        store
            .find_reference(&site_id, "mxc://hs/never-observed")
            .await
            .unwrap()
            .is_none()
    );
}

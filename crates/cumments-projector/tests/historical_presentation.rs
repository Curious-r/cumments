//! Integration tests for Historical Room-State Resolution (DAG context).
//!
//! Verifies:
//! 1. Historical snapshot is immutable when author profile changes later.
//! 2. Arrival-order inversion resolves DAG context, not arrival-time room state.
//! 3. Leave after message preserves historical snapshot.
//! 4. Leave before message resolves effective DAG state without falling back to old presentation.
//! 5. Lifecycle (join P1 -> join P2 -> message at P2 -> leave -> join P3) maintains P2 snapshot while live view shows P3.
//! 6. Resolver failure fails message processing explicitly without fallback to room_members.
//! 7. Missing resolver fails message processing explicitly.
//! 8. Durable MediaReference resolution handles existing references, local uploads, and degraded unknown avatars without speculative IDs.
//! 9. Rebuild determinism: chronological vs reverse replay produces identical stored snapshots.

use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::broadcast;

use cumments_core::media_reference::MediaReferenceSource;
use cumments_core::media_upload::MediaUploadIdempotencyInput;
use cumments_core::models::{
    Content, MemberPresentation, PageSlug, RoomIdentity, SiteId, TextContent, TextStyle,
};
use cumments_core::ports::{
    MediaReferenceStore, MessageStore, RegistryStore, RoomStore, SiteStore,
};
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::{ParsedRoomMessage, ParsedRoomState};
use cumments_store::DbStore;
use cumments_store::entities::messages;
use cumments_test_utils::TestDriver;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::json;

mod common;

static TEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn test_db_url(name: &str) -> String {
    let id = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-hist-pres-{}-{}-{}.db",
        name,
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn create_processor_with_driver(
    store: Arc<DbStore>,
    driver: Option<Arc<TestDriver>>,
) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    let historical_state_resolver = driver
        .clone()
        .map(|d| d as Arc<dyn cumments_core::ports::HistoricalRoomStateResolver>);
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
        driver: driver.map(|d| d as Arc<dyn cumments_core::ports::MatrixDriver>),
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(Notify::new()),
        projection_notify: Arc::new(Notify::new()),
        server_name: Some("hs".to_string()),
        media_reference_store: Some(store.clone()),
        historical_state_resolver,
    })
}

fn make_room_message(
    room_id: &str,
    event_id: &str,
    sender: &str,
    body: &str,
    ts: i64,
    site_id: &str,
    page_slug: &str,
) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: room_id.to_string(),
        event_id: event_id.to_string(),
        event_type: "m.room.message".to_string(),
        sender: sender.to_string(),
        content: Content::Text(TextContent {
            body: body.to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        author_public_key: None,
        author_signature: None,
        author_challenge: None,
        is_virtual_user_sender: false,
        submission_id: None,
        reply_to: None,
        thread_root: None,
        origin_server_ts: ts,
        relates_to: None,
        room_identity: Some(RoomIdentity {
            site_id: site_id.to_string(),
            page_slug: page_slug.to_string(),
        }),
        raw_content: json!({ "msgtype": "m.text", "body": body }),
    }
}

fn make_member_state(
    room_id: &str,
    event_id: &str,
    user_id: &str,
    membership: &str,
    display_name: Option<&str>,
    avatar_url: Option<&str>,
    ts: i64,
) -> ParsedRoomState {
    let mut content = json!({ "membership": membership });
    if let Some(name) = display_name {
        content["displayname"] = json!(name);
    }
    if let Some(avatar) = avatar_url {
        content["avatar_url"] = json!(avatar);
    }
    ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: event_id.to_string(),
        sender: user_id.to_string(),
        event_type: "m.room.member".to_string(),
        state_key: user_id.to_string(),
        origin_server_ts: ts,
        content,
    }
}

async fn get_raw_message(store: &DbStore, event_id: &str) -> Option<messages::Model> {
    messages::Entity::find()
        .filter(messages::Column::EventId.eq(event_id))
        .one(store.connection())
        .await
        .expect("query raw message")
}

#[tokio::test]
async fn historical_snapshot_is_immutable_when_author_profile_changes_later() {
    let db_url = test_db_url("immutable_on_profile_change");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let driver = Arc::new(TestDriver::new());

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-1";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // 1. Initial member state P1
    processor
        .process_room_state(make_member_state(
            room_id,
            "$member-1",
            user_id,
            "join",
            Some("Alice P1"),
            Some("mxc://hs/alice1"),
            1000,
        ))
        .await
        .expect("process member 1");

    // 2. Set resolver for message event_id to return P1
    driver
        .set_historical_member_presentation(
            room_id,
            event_id,
            user_id,
            Some(MemberPresentation {
                display_name: Some("Alice P1".to_string()),
                avatar_url: Some("mxc://hs/alice1".to_string()),
                media_reference: None,
            }),
        )
        .await;

    // 3. Process message at $msg-1
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Hello world",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process room message");

    // 4. Update profile to P2 later
    processor
        .process_room_state(make_member_state(
            room_id,
            "$member-2",
            user_id,
            "join",
            Some("Alice P2"),
            Some("mxc://hs/alice2"),
            2000,
        ))
        .await
        .expect("process member 2");

    // Check room_members projection is updated to P2
    let member = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.display_name.as_deref(), Some("Alice P2"));
    assert_eq!(member.avatar_url.as_deref(), Some("mxc://hs/alice2"));

    // Check raw row in `messages` table retains immutable historical snapshot P1
    let raw_msg = get_raw_message(&store, event_id)
        .await
        .expect("message row exists");
    assert_eq!(raw_msg.author_display_name.as_deref(), Some("Alice P1"));
    assert_eq!(
        raw_msg.author_avatar_url.as_deref(),
        Some("mxc://hs/alice1")
    );
}

#[tokio::test]
async fn arrival_order_inversion_resolves_dag_context_not_arrival_state() {
    let db_url = test_db_url("arrival_order_inversion");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let driver = Arc::new(TestDriver::new());

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-historic";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // 1. Inverted arrival: Projector receives NEWER state P2 FIRST
    processor
        .process_room_state(make_member_state(
            room_id,
            "$member-p2",
            user_id,
            "join",
            Some("Alice Newer P2"),
            Some("mxc://hs/newer-avatar"),
            2000,
        ))
        .await
        .expect("process member 2");

    // Current room_members projection is now P2
    let current_member = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(
        current_member.display_name.as_deref(),
        Some("Alice Newer P2")
    );

    // 2. Resolver is configured to return P1 for $msg-historic (its DAG context)
    driver
        .set_historical_member_presentation(
            room_id,
            event_id,
            user_id,
            Some(MemberPresentation {
                display_name: Some("Alice Historic P1".to_string()),
                avatar_url: Some("mxc://hs/historic-avatar".to_string()),
                media_reference: None,
            }),
        )
        .await;

    // 3. Process historical message
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Comment from the past",
            1000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process room message");

    // 4. Verify message row in `messages` stores P1, NOT the current arrival-time projection P2!
    let raw_msg = get_raw_message(&store, event_id)
        .await
        .expect("message row exists");
    assert_eq!(
        raw_msg.author_display_name.as_deref(),
        Some("Alice Historic P1")
    );
    assert_eq!(
        raw_msg.author_avatar_url.as_deref(),
        Some("mxc://hs/historic-avatar")
    );
}

#[tokio::test]
async fn author_leave_after_message_preserves_historical_snapshot() {
    let db_url = test_db_url("leave_after_message");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let driver = Arc::new(TestDriver::new());

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-before-leave";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // 1. Join with P1
    processor
        .process_room_state(make_member_state(
            room_id,
            "$join-1",
            user_id,
            "join",
            Some("Alice In Room"),
            Some("mxc://hs/alice-avatar"),
            1000,
        ))
        .await
        .expect("join");

    // 2. Set resolver for message
    driver
        .set_historical_member_presentation(
            room_id,
            event_id,
            user_id,
            Some(MemberPresentation {
                display_name: Some("Alice In Room".to_string()),
                avatar_url: Some("mxc://hs/alice-avatar".to_string()),
                media_reference: None,
            }),
        )
        .await;

    // 3. Process message
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "I am here",
            1100,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process message");

    // 4. Alice leaves
    processor
        .process_room_state(make_member_state(
            room_id, "$leave-1", user_id, "leave", None, None, 2000,
        ))
        .await
        .expect("leave");

    // Verify stored message snapshot is still P1
    let raw_msg = get_raw_message(&store, event_id)
        .await
        .expect("message row exists");
    assert_eq!(
        raw_msg.author_display_name.as_deref(),
        Some("Alice In Room")
    );
    assert_eq!(
        raw_msg.author_avatar_url.as_deref(),
        Some("mxc://hs/alice-avatar")
    );
}

#[tokio::test]
async fn leave_before_message_resolves_effective_dag_state_not_previous_membership() {
    let db_url = test_db_url("leave_before_message");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let driver = Arc::new(TestDriver::new());

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-departed";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // 1. Join with P1 earlier
    processor
        .process_room_state(make_member_state(
            room_id,
            "$join-1",
            user_id,
            "join",
            Some("Old Alice"),
            Some("mxc://hs/old-avatar"),
            1000,
        ))
        .await
        .expect("join");

    // 2. Leave
    processor
        .process_room_state(make_member_state(
            room_id, "$leave-1", user_id, "leave", None, None, 2000,
        ))
        .await
        .expect("leave");

    // 3. Effective state at DAG context for $msg-departed is None (left / no presentation)
    driver
        .set_historical_member_presentation(room_id, event_id, user_id, None)
        .await;

    // 4. Process message
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Ghost message",
            2100,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process message");

    // Stored message must resolve to None, not resurrecting "Old Alice" from previous projection!
    let raw_msg = get_raw_message(&store, event_id)
        .await
        .expect("message row exists");
    assert_eq!(raw_msg.author_display_name, None);
    assert_eq!(raw_msg.author_avatar_url, None);
    assert_eq!(raw_msg.author_media_reference, None);
}

#[tokio::test]
async fn join_update_leave_rejoin_lifecycle_snapshot_and_live_presentation() {
    let db_url = test_db_url("lifecycle_snapshot_and_live");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let driver = Arc::new(TestDriver::new());

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-at-p2";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // 1. Join with P1
    processor
        .process_room_state(make_member_state(
            room_id,
            "$m-1",
            user_id,
            "join",
            Some("Alice P1"),
            Some("mxc://hs/p1"),
            1000,
        ))
        .await
        .expect("p1");

    // 2. Update to P2
    processor
        .process_room_state(make_member_state(
            room_id,
            "$m-2",
            user_id,
            "join",
            Some("Alice P2"),
            Some("mxc://hs/p2"),
            2000,
        ))
        .await
        .expect("p2");

    // Resolver returns P2 for $msg-at-p2
    driver
        .set_historical_member_presentation(
            room_id,
            event_id,
            user_id,
            Some(MemberPresentation {
                display_name: Some("Alice P2".to_string()),
                avatar_url: Some("mxc://hs/p2".to_string()),
                media_reference: None,
            }),
        )
        .await;

    // 3. Message sent at P2
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Message at P2",
            2100,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("message at p2");

    // 4. Leave
    processor
        .process_room_state(make_member_state(
            room_id, "$m-3", user_id, "leave", None, None, 3000,
        ))
        .await
        .expect("leave");

    // 5. Rejoin with P3
    processor
        .process_room_state(make_member_state(
            room_id,
            "$m-4",
            user_id,
            "join",
            Some("Alice P3"),
            Some("mxc://hs/p3"),
            4000,
        ))
        .await
        .expect("rejoin p3");

    // Verify stored snapshot is strictly P2
    let raw_msg = get_raw_message(&store, event_id)
        .await
        .expect("message row exists");
    assert_eq!(raw_msg.author_display_name.as_deref(), Some("Alice P2"));
    assert_eq!(raw_msg.author_avatar_url.as_deref(), Some("mxc://hs/p2"));

    // Verify live view via MessageStore::get_message hydrates to current P3
    let hydrated = store
        .get_message(event_id)
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(hydrated.author.display_name.as_deref(), Some("Alice P3"));
    assert_eq!(hydrated.author.avatar_url.as_deref(), Some("mxc://hs/p3"));
}

#[tokio::test]
async fn resolver_failure_fails_processing_explicitly_without_room_members_fallback() {
    let db_url = test_db_url("resolver_failure_no_fallback");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let driver = Arc::new(TestDriver::new());

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-fail";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // Member is in room_members projection with P1
    processor
        .process_room_state(make_member_state(
            room_id,
            "$member-1",
            user_id,
            "join",
            Some("Fallback Alice"),
            Some("mxc://hs/fallback"),
            1000,
        ))
        .await
        .expect("join");

    // Force resolver to fail
    driver.set_fail_historical_resolution(true).await;

    // Processing must fail!
    let res = processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Must fail",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await;
    assert!(res.is_err(), "Must error on resolver failure");

    // Message must NOT be saved into messages table (no fallback!)
    let raw_msg = get_raw_message(&store, event_id).await;
    assert!(
        raw_msg.is_none(),
        "Message must not be saved when resolver fails"
    );
}

#[tokio::test]
async fn missing_resolver_fails_explicitly() {
    let db_url = test_db_url("missing_resolver");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-no-resolver";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    // Create processor with historical_state_resolver: None
    let processor = create_processor_with_driver(store.clone(), None);

    let res = processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "No resolver",
            1000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await;

    assert!(res.is_err(), "Must fail when resolver is missing");
    let err = res.unwrap_err().to_string();
    assert!(err.contains("HistoricalRoomStateResolver is unavailable"));
}

#[tokio::test]
async fn durable_media_reference_resolution_and_degraded_unknown_avatar() {
    let db_url = test_db_url("media_ref_and_degraded");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let driver = Arc::new(TestDriver::new());

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor_with_driver(store.clone(), Some(driver.clone()));

    // 1. Existing reference mapping in MediaReferenceStore
    let known_mxc = "mxc://hs/known-avatar";
    let existing_ref = store
        .get_or_create_reference(&site_id, known_mxc, MediaReferenceSource::Cumments)
        .await
        .expect("create existing reference");

    driver
        .set_historical_member_presentation(
            room_id,
            "$msg-known",
            user_id,
            Some(MemberPresentation {
                display_name: Some("Alice Known".to_string()),
                avatar_url: Some(known_mxc.to_string()),
                media_reference: None,
            }),
        )
        .await;

    processor
        .process_room_message(make_room_message(
            room_id,
            "$msg-known",
            user_id,
            "With known avatar",
            1000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process known");

    let known_msg = get_raw_message(&store, "$msg-known")
        .await
        .expect("known msg exists");
    assert_eq!(
        known_msg.author_media_reference.as_deref(),
        Some(existing_ref.as_str())
    );

    // 2. Local upload record mapping
    let upload_mxc = "mxc://hs/upload-avatar";
    store
        .save_media_upload_idempotent(
            upload_mxc,
            "author-pubkey",
            site_id.as_str(),
            Some(page_slug.as_str()),
            &MediaUploadIdempotencyInput {
                key: "key-1".to_string(),
                request_fingerprint: "fp-1".to_string(),
            },
        )
        .await
        .expect("create upload record");

    driver
        .set_historical_member_presentation(
            room_id,
            "$msg-upload",
            user_id,
            Some(MemberPresentation {
                display_name: Some("Alice Upload".to_string()),
                avatar_url: Some(upload_mxc.to_string()),
                media_reference: None,
            }),
        )
        .await;

    processor
        .process_room_message(make_room_message(
            room_id,
            "$msg-upload",
            user_id,
            "With uploaded avatar",
            1100,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process upload");

    let upload_msg = get_raw_message(&store, "$msg-upload")
        .await
        .expect("upload msg exists");
    assert!(
        upload_msg.author_media_reference.is_some(),
        "Upload-backed avatar must resolve a durable MediaReference"
    );

    // 3. Unknown degraded avatar (neither reference nor upload record exists)
    driver
        .set_historical_member_presentation(
            room_id,
            "$msg-unknown",
            user_id,
            Some(MemberPresentation {
                display_name: Some("Alice Unknown".to_string()),
                avatar_url: Some("mxc://hs/unknown-external-avatar".to_string()),
                media_reference: None,
            }),
        )
        .await;

    processor
        .process_room_message(make_room_message(
            room_id,
            "$msg-unknown",
            user_id,
            "With unknown avatar",
            1200,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process unknown");

    let unknown_msg = get_raw_message(&store, "$msg-unknown")
        .await
        .expect("unknown msg exists");
    assert_eq!(
        unknown_msg.author_avatar_url.as_deref(),
        Some("mxc://hs/unknown-external-avatar")
    );
    // Crucial requirement: Media reference must remain None without allocating speculative IDs
    assert_eq!(unknown_msg.author_media_reference, None);
}

#[tokio::test]
async fn rebuild_determinism_chronological_vs_reverse_order() {
    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";

    // Setup Store A (chronological replay)
    let db_url_a = test_db_url("rebuild_chrono");
    let store_a = Arc::new(DbStore::connect(&db_url_a).await.expect("connect a"));
    let driver_a = Arc::new(TestDriver::new());

    store_a
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store_a
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor_a = create_processor_with_driver(store_a.clone(), Some(driver_a.clone()));

    // Setup Store B (reverse replay)
    let db_url_b = test_db_url("rebuild_reverse");
    let store_b = Arc::new(DbStore::connect(&db_url_b).await.expect("connect b"));
    let driver_b = Arc::new(TestDriver::new());

    store_b
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store_b
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor_b = create_processor_with_driver(store_b.clone(), Some(driver_b.clone()));

    // Configure presentations for both drivers
    let pres_1 = MemberPresentation {
        display_name: Some("Alice Early".to_string()),
        avatar_url: Some("mxc://hs/early".to_string()),
        media_reference: None,
    };
    let pres_2 = MemberPresentation {
        display_name: Some("Alice Later".to_string()),
        avatar_url: Some("mxc://hs/later".to_string()),
        media_reference: None,
    };

    driver_a
        .set_historical_member_presentation(room_id, "$msg-1", user_id, Some(pres_1.clone()))
        .await;
    driver_a
        .set_historical_member_presentation(room_id, "$msg-2", user_id, Some(pres_2.clone()))
        .await;

    driver_b
        .set_historical_member_presentation(room_id, "$msg-1", user_id, Some(pres_1))
        .await;
    driver_b
        .set_historical_member_presentation(room_id, "$msg-2", user_id, Some(pres_2))
        .await;

    // Chronological replay in A: msg1 then msg2
    processor_a
        .process_room_message(make_room_message(
            room_id,
            "$msg-1",
            user_id,
            "First",
            1000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("msg 1 in a");
    processor_a
        .process_room_message(make_room_message(
            room_id,
            "$msg-2",
            user_id,
            "Second",
            2000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("msg 2 in a");

    // Reverse replay in B: msg2 then msg1
    processor_b
        .process_room_message(make_room_message(
            room_id,
            "$msg-2",
            user_id,
            "Second",
            2000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("msg 2 in b");
    processor_b
        .process_room_message(make_room_message(
            room_id,
            "$msg-1",
            user_id,
            "First",
            1000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("msg 1 in b");

    // Compare stored message snapshots between A and B
    let msg1_a = get_raw_message(&store_a, "$msg-1").await.expect("msg1_a");
    let msg1_b = get_raw_message(&store_b, "$msg-1").await.expect("msg1_b");

    assert_eq!(msg1_a.author_display_name, msg1_b.author_display_name);
    assert_eq!(msg1_a.author_avatar_url, msg1_b.author_avatar_url);
    assert_eq!(msg1_a.author_media_reference, msg1_b.author_media_reference);
    assert_eq!(msg1_a.author_display_name.as_deref(), Some("Alice Early"));

    let msg2_a = get_raw_message(&store_a, "$msg-2").await.expect("msg2_a");
    let msg2_b = get_raw_message(&store_b, "$msg-2").await.expect("msg2_b");

    assert_eq!(msg2_a.author_display_name, msg2_b.author_display_name);
    assert_eq!(msg2_a.author_avatar_url, msg2_b.author_avatar_url);
    assert_eq!(msg2_a.author_media_reference, msg2_b.author_media_reference);
    assert_eq!(msg2_a.author_display_name.as_deref(), Some("Alice Later"));
}

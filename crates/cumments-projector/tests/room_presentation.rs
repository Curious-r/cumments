//! Integration tests for Room Presentation and Leave Semantics.
//!
//! Verifies:
//! 1. Leave preserves latest usable presentation across messages.
//! 2. Ban preserves latest usable presentation across messages.
//! 3. Join update (`join -> join`) updates presentation.
//! 4. Leave after update (`join -> join -> leave`) preserves the updated presentation.
//! 5. Leave then join (`join -> leave -> join`) tracks the new presentation on rejoin.
//! 6. Consecutive join events (`join -> join`) are not treated as rejoins.
//! 7. Member-event processing resolves avatar through durable MediaReference mapping.
//! 8. Projection rebuild deterministically reuses stable MediaReference mappings.
//! 9. Existing external provenance is preserved during member observation.
//! 10. Message author enrichment does not require `membership == join`.
//! 11. Historical author snapshot remains a fallback when no usable room presentation exists.

use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::broadcast;

use cumments_core::media_reference::MediaReferenceSource;
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, Message, MessageStatus, PageSlug, SiteId, TextContent,
    TextStyle,
};
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
        "cumments-room-pres-{}-{}.db",
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
        historical_state_resolver: None,
    })
}

static TEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn setup_test_environment() -> (EventProcessor, Arc<DbStore>, Arc<DbStore>) {
    let id = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let db_url = test_db_url(&format!("ordering-{}", id));
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure test-site");

    store
        .ensure_site_exists("example.com", "!example-space:hs")
        .await
        .expect("ensure example.com");

    let processor = create_processor(store.clone());
    (processor, store.clone(), store)
}

fn create_test_message(
    event_id: &str,
    room_id: &str,
    sender_mxid: &str,
    snapshot_name: Option<&str>,
    snapshot_avatar: Option<&str>,
) -> Message {
    Message {
        event_id: event_id.to_string(),
        site_id: "test-site".to_string(),
        page_slug: "test-page".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: snapshot_name.map(str::to_string),
            avatar_url: snapshot_avatar.map(str::to_string),
            media_reference: None,
            public_key: Some("test_pubkey_1234567890".to_string()),
            mxid: None,
        },
        content: Content::Text(TextContent {
            body: "test comment".to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        matrix_event_type: "m.room.message".to_string(),
        timestamp: chrono::Utc::now(),
        edited_at: None,
        reply_to: None,
        thread_root: None,
        submission_id: Some(1),
        status: MessageStatus::Active,
        redacted_at: None,
        redacted_by: None,
        reactions: Vec::new(),
        thread_summary: None,
        room_id: room_id.to_string(),
        sender_mxid: sender_mxid.to_string(),
        raw_content: json!({ "msgtype": "m.text", "body": "test comment" }),
    }
}

#[tokio::test]
async fn leave_preserves_presentation_and_message_enrichment() {
    let db_url = test_db_url("leave_preserves_presentation");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!comments:hs";
    let user_id = "@alice:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());

    // 1. Join with presentation P1
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Alice",
                "avatar_url": "mxc://hs/alice-avatar",
            }),
        })
        .await
        .expect("process join");

    let member = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.display_name.as_deref(), Some("Alice"));
    assert_eq!(member.avatar_url.as_deref(), Some("mxc://hs/alice-avatar"));
    assert_eq!(member.membership, "join");

    // Save a message from Alice with snapshot fallback
    let msg = create_test_message("$msg-1", room_id, user_id, Some("Alice Original"), None);
    store.save_message(&msg).await.expect("save message");

    let fetched = store
        .get_message("$msg-1")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(fetched.author.display_name.as_deref(), Some("Alice"));
    assert_eq!(
        fetched.author.avatar_url.as_deref(),
        Some("mxc://hs/alice-avatar")
    );

    // 2. Member leaves room (leave event lacks profile fields)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$leave-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "leave",
            }),
        })
        .await
        .expect("process leave");

    // Projection retains latest usable presentation on leave
    let member_after_leave = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member_after_leave.membership, "leave");
    assert_eq!(member_after_leave.display_name.as_deref(), Some("Alice"));
    assert_eq!(
        member_after_leave.avatar_url.as_deref(),
        Some("mxc://hs/alice-avatar")
    );

    // Message enrichment still renders the latest usable presentation P1!
    let fetched_after_leave = store
        .get_message("$msg-1")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(
        fetched_after_leave.author.display_name.as_deref(),
        Some("Alice"),
        "leaving room must not snap display name back to historical fallback"
    );
    assert_eq!(
        fetched_after_leave.author.avatar_url.as_deref(),
        Some("mxc://hs/alice-avatar"),
        "leaving room must not erase author avatar"
    );

    // Author display name query for edits also retains latest presentation
    let current_name = store
        .get_author_display_name("$msg-1")
        .await
        .expect("get author display name");
    assert_eq!(current_name.flatten().as_deref(), Some("Alice"));
}

#[tokio::test]
async fn ban_preserves_presentation_and_message_enrichment() {
    let db_url = test_db_url("ban_preserves_presentation");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!comments:hs";
    let user_id = "@spammer:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());

    // 1. Join with P1
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Mallory",
                "avatar_url": "mxc://hs/mallory-avatar",
            }),
        })
        .await
        .expect("process join");

    let msg = create_test_message("$msg-ban", room_id, user_id, Some("Mallory Orig"), None);
    store.save_message(&msg).await.expect("save message");

    // 2. Ban event issued by moderator
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$ban-1".to_string(),
            sender: "@mod:hs".to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "ban",
                "reason": "violating community guidelines",
            }),
        })
        .await
        .expect("process ban");

    let member = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.membership, "ban");
    assert_eq!(member.display_name.as_deref(), Some("Mallory"));
    assert_eq!(
        member.avatar_url.as_deref(),
        Some("mxc://hs/mallory-avatar")
    );

    // Message enrichment still shows Mallory's latest presentation
    let fetched = store
        .get_message("$msg-ban")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(fetched.author.display_name.as_deref(), Some("Mallory"));
    assert_eq!(
        fetched.author.avatar_url.as_deref(),
        Some("mxc://hs/mallory-avatar")
    );
}

#[tokio::test]
async fn join_update_and_leave_after_update() {
    let db_url = test_db_url("join_update_and_leave");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!comments:hs";
    let user_id = "@bob:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());

    // 1. Initial join with P1
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Bob Original",
                "avatar_url": "mxc://hs/bob-1",
            }),
        })
        .await
        .expect("process join 1");

    let msg = create_test_message("$msg-bob", room_id, user_id, Some("Bob Snapshot"), None);
    store.save_message(&msg).await.expect("save message");

    // 2. Join update (join -> join): rename / avatar change
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-2".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1500,
            content: json!({
                "membership": "join",
                "displayname": "Bob Updated",
                "avatar_url": "mxc://hs/bob-2",
            }),
        })
        .await
        .expect("process join 2");

    let member = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.membership, "join");
    assert_eq!(member.display_name.as_deref(), Some("Bob Updated"));
    assert_eq!(member.avatar_url.as_deref(), Some("mxc://hs/bob-2"));

    // Messages now render P2
    let fetched = store
        .get_message("$msg-bob")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(fetched.author.display_name.as_deref(), Some("Bob Updated"));
    assert_eq!(fetched.author.avatar_url.as_deref(), Some("mxc://hs/bob-2"));

    // 3. Leave after update (join -> join -> leave)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$leave-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "leave",
            }),
        })
        .await
        .expect("process leave");

    // P2 remains current presentation across historical comments
    let member_after_leave = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member_after_leave.membership, "leave");
    assert_eq!(
        member_after_leave.display_name.as_deref(),
        Some("Bob Updated")
    );
    assert_eq!(
        member_after_leave.avatar_url.as_deref(),
        Some("mxc://hs/bob-2")
    );

    let fetched_after_leave = store
        .get_message("$msg-bob")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(
        fetched_after_leave.author.display_name.as_deref(),
        Some("Bob Updated")
    );
    assert_eq!(
        fetched_after_leave.author.avatar_url.as_deref(),
        Some("mxc://hs/bob-2")
    );
}

#[tokio::test]
async fn leave_then_join_rejoin_lifecycle() {
    let db_url = test_db_url("leave_then_join");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!comments:hs";
    let user_id = "@charlie:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());

    // 1. Join with P1
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Charlie 1",
            }),
        })
        .await
        .expect("join 1");

    // 2. Leave
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$leave-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "leave",
            }),
        })
        .await
        .expect("leave");

    let left = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .unwrap();
    assert_eq!(left.membership, "leave");
    assert_eq!(left.display_name.as_deref(), Some("Charlie 1"));

    // 3. Rejoin with P3 (actual membership transition)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$rejoin-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 3000,
            content: json!({
                "membership": "join",
                "displayname": "Charlie Rejoined",
                "avatar_url": "mxc://hs/charlie-rejoin",
            }),
        })
        .await
        .expect("rejoin");

    let rejoined = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .unwrap();
    assert_eq!(rejoined.membership, "join");
    assert_eq!(rejoined.display_name.as_deref(), Some("Charlie Rejoined"));
    assert_eq!(
        rejoined.avatar_url.as_deref(),
        Some("mxc://hs/charlie-rejoin")
    );
}

#[tokio::test]
async fn media_reference_mapping_and_projection_rebuild_determinism() {
    let db_url = test_db_url("media_ref_rebuild_determinism");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!comments:hs";
    let user_id = "@dave:hs";
    let mxc_uri = "mxc://hs/dave-avatar-pic";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    // Record authoritatively in media_uploads that this avatar was uploaded by Cumments for this site
    store
        .record_media_upload(mxc_uri, "dave-key", site_id.as_str(), None)
        .await
        .expect("record media upload");

    let processor = create_processor(store.clone());

    // 1. Process member join event with avatar
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-dave".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Dave",
                "avatar_url": mxc_uri,
            }),
        })
        .await
        .expect("process dave join");

    let member = store
        .get_member(room_id, user_id)
        .await
        .expect("get dave")
        .expect("dave exists");
    assert_eq!(member.display_name.as_deref(), Some("Dave"));
    assert_eq!(member.avatar_url.as_deref(), Some(mxc_uri));

    let allocated_ref = member
        .media_reference
        .expect("room_member must have media_reference");
    assert!(allocated_ref.as_str().starts_with("cumments-media:"));

    // Verify durable media_references record
    let record = store
        .get_record(&site_id, &allocated_ref)
        .await
        .expect("get record")
        .expect("record exists");
    assert_eq!(record.mxc_uri, mxc_uri);
    assert!(!record.is_external);

    // 2. Member leaves: media_reference is preserved on leave
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$leave-dave".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "leave",
            }),
        })
        .await
        .expect("process dave leave");

    let member_left = store
        .get_member(room_id, user_id)
        .await
        .expect("get dave")
        .expect("dave exists");
    assert_eq!(member_left.membership, "leave");
    assert_eq!(member_left.display_name.as_deref(), Some("Dave"));
    assert_eq!(member_left.avatar_url.as_deref(), Some(mxc_uri));
    assert_eq!(member_left.media_reference, Some(allocated_ref.clone()));

    // 3. Simulate projection reset / rebuild: wipe room_members
    // but keep durable media_references intact.
    store
        .delete_member(room_id, user_id)
        .await
        .expect("wipe member projection");

    // Replay the join event
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-dave".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Dave",
                "avatar_url": mxc_uri,
            }),
        })
        .await
        .expect("replay dave join");

    let rebuilt_member = store
        .get_member(room_id, user_id)
        .await
        .expect("get dave")
        .expect("dave exists");
    assert_eq!(
        rebuilt_member.media_reference,
        Some(allocated_ref),
        "projection rebuild must reuse the exact same stable MediaReference"
    );
}

#[tokio::test]
async fn external_provenance_preserved_on_room_member_observation() {
    let db_url = test_db_url("external_provenance_preserved");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!comments:hs";
    let user_id = "@external_user:hs";
    let ext_mxc = "mxc://hs/ext-avatar-999";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    // Pre-create external mapping (discovered through external profile reconciler)
    let ext_ref = store
        .get_or_create_reference(&site_id, ext_mxc, MediaReferenceSource::External)
        .await
        .expect("create external reference");

    let record_before = store
        .get_record(&site_id, &ext_ref)
        .await
        .expect("get record")
        .expect("exists");
    assert!(record_before.is_external);

    let processor = create_processor(store.clone());

    // Observe ordinary m.room.member event using this external avatar
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-ext".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "External User",
                "avatar_url": ext_mxc,
            }),
        })
        .await
        .expect("process join");

    let member = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.media_reference, Some(ext_ref.clone()));

    // Provenance must remain is_external = true
    let record_after = store
        .get_record(&site_id, &ext_ref)
        .await
        .expect("get record")
        .expect("exists");
    assert!(
        record_after.is_external,
        "existing external provenance must be preserved across room member observation"
    );
}

#[tokio::test]
async fn historical_fallback_when_no_usable_current_room_presentation() {
    let db_url = test_db_url("historical_fallback");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!comments:hs";
    let user_id = "@author_no_state:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    // Save a message whose author has no room_members state at all
    let msg = create_test_message(
        "$msg-historical-only",
        room_id,
        user_id,
        Some("Historical Name"),
        Some("mxc://hs/historical-avatar"),
    );
    store.save_message(&msg).await.expect("save message");

    let fetched = store
        .get_message("$msg-historical-only")
        .await
        .expect("get message")
        .expect("message exists");

    // Falls back to historical author snapshot
    assert_eq!(
        fetched.author.display_name.as_deref(),
        Some("Historical Name")
    );
    assert_eq!(
        fetched.author.avatar_url.as_deref(),
        Some("mxc://hs/historical-avatar")
    );
}

#[tokio::test]
async fn join_to_join_is_not_rejoin() {
    let db_url = test_db_url("join_to_join_not_rejoin");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!comments:hs";
    let user_id = "@user_rename:hs";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let processor = create_processor(store.clone());

    // 1. First join event
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-rename-1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Name V1",
                "avatar_url": "mxc://hs/avatar-v1",
            }),
        })
        .await
        .expect("join 1");

    let member_v1 = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .unwrap();
    assert_eq!(member_v1.membership, "join");
    assert_eq!(member_v1.display_name.as_deref(), Some("Name V1"));
    assert_eq!(member_v1.avatar_url.as_deref(), Some("mxc://hs/avatar-v1"));

    // 2. Second join event (profile update: join -> join)
    // Frozen spec §9.2: join -> join is a profile update, not an actual room rejoin.
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-rename-2".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "Name V2",
                "avatar_url": "mxc://hs/avatar-v2",
            }),
        })
        .await
        .expect("join 2");

    let member_v2 = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .unwrap();
    assert_eq!(member_v2.membership, "join");
    assert_eq!(member_v2.display_name.as_deref(), Some("Name V2"));
    assert_eq!(member_v2.avatar_url.as_deref(), Some("mxc://hs/avatar-v2"));

    // 3. Third join event updating only displayname
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-rename-3".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 3000,
            content: json!({
                "membership": "join",
                "displayname": "Name V3",
                "avatar_url": "mxc://hs/avatar-v2",
            }),
        })
        .await
        .expect("join 3");

    let member_v3 = store
        .get_member(room_id, user_id)
        .await
        .expect("get member")
        .unwrap();
    assert_eq!(member_v3.membership, "join");
    assert_eq!(member_v3.display_name.as_deref(), Some("Name V3"));
    assert_eq!(member_v3.avatar_url.as_deref(), Some("mxc://hs/avatar-v2"));
}

#[tokio::test]
async fn message_author_enrichment_does_not_require_join() {
    let db_url = test_db_url("enrichment_no_join_gate");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
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

    let user_active = "@user_active:hs";
    let user_left = "@user_left:hs";
    let user_banned = "@user_banned:hs";
    let user_nostate = "@user_nostate:hs";

    // Setup active member
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$j-active".to_string(),
            sender: user_active.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_active.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Active Alice",
                "avatar_url": "mxc://hs/active",
            }),
        })
        .await
        .expect("process active");

    // Setup left member (join then leave)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$j-left".to_string(),
            sender: user_left.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_left.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Left Bob",
                "avatar_url": "mxc://hs/left",
            }),
        })
        .await
        .expect("process left join");
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$l-left".to_string(),
            sender: user_left.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_left.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "leave",
            }),
        })
        .await
        .expect("process left leave");

    // Setup banned member (join then ban)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$j-ban".to_string(),
            sender: user_banned.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_banned.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Banned Charlie",
                "avatar_url": "mxc://hs/banned",
            }),
        })
        .await
        .expect("process ban join");
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$b-ban".to_string(),
            sender: "@mod:hs".to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_banned.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "ban",
                "reason": "trolling",
            }),
        })
        .await
        .expect("process ban");

    // Save messages for each author with their original historical snapshots
    let msg_active =
        create_test_message("$m-active", room_id, user_active, Some("Old Alice"), None);
    let msg_left = create_test_message("$m-left", room_id, user_left, Some("Old Bob"), None);
    let msg_banned =
        create_test_message("$m-banned", room_id, user_banned, Some("Old Charlie"), None);
    let msg_nostate = create_test_message(
        "$m-nostate",
        room_id,
        user_nostate,
        Some("Historical Dave"),
        Some("mxc://hs/old-dave"),
    );

    store.save_message(&msg_active).await.expect("save active");
    store.save_message(&msg_left).await.expect("save left");
    store.save_message(&msg_banned).await.expect("save banned");
    store
        .save_message(&msg_nostate)
        .await
        .expect("save nostate");

    // Fetch batch of messages through get_messages
    let page = store
        .get_messages(&site_id, &page_slug, 10, 0, None)
        .await
        .expect("get messages");
    assert_eq!(page.items.len(), 4);

    let by_id: std::collections::HashMap<String, Message> = page
        .items
        .into_iter()
        .map(|m| (m.event_id.clone(), m))
        .collect();

    // 1. Active member uses live presentation
    let m1 = by_id.get("$m-active").unwrap();
    assert_eq!(m1.author.display_name.as_deref(), Some("Active Alice"));
    assert_eq!(m1.author.avatar_url.as_deref(), Some("mxc://hs/active"));

    // 2. Departed member uses latest usable presentation (NOT stripped by membership != join!)
    let m2 = by_id.get("$m-left").unwrap();
    assert_eq!(
        m2.author.display_name.as_deref(),
        Some("Left Bob"),
        "departed author must retain latest usable display name"
    );
    assert_eq!(
        m2.author.avatar_url.as_deref(),
        Some("mxc://hs/left"),
        "departed author must retain latest usable avatar"
    );

    // 3. Banned member uses latest usable presentation (NOT stripped by membership != join!)
    let m3 = by_id.get("$m-banned").unwrap();
    assert_eq!(
        m3.author.display_name.as_deref(),
        Some("Banned Charlie"),
        "banned author must retain latest usable display name"
    );
    assert_eq!(
        m3.author.avatar_url.as_deref(),
        Some("mxc://hs/banned"),
        "banned author must retain latest usable avatar"
    );

    // 4. Member without room state falls back to historical snapshot
    let m4 = by_id.get("$m-nostate").unwrap();
    assert_eq!(
        m4.author.display_name.as_deref(),
        Some("Historical Dave"),
        "author without room member state must fall back to historical display name"
    );
    assert_eq!(
        m4.author.avatar_url.as_deref(),
        Some("mxc://hs/old-dave"),
        "author without room member state must fall back to historical avatar"
    );
}

#[tokio::test]
async fn out_of_order_newer_join_then_older_join_ignores_older() {
    let (processor, store, _media_store) = setup_test_environment().await;
    let room_id = "!room-ordering-1:hs";
    let user_id = "@user1:hs";

    // 1. Newer join P2 @ 2000
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-p2".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "P2",
                "avatar_url": "mxc://hs/p2",
            }),
        })
        .await
        .expect("process p2");

    let member = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(member.membership, "join");
    assert_eq!(member.display_name.as_deref(), Some("P2"));
    assert_eq!(member.origin_server_ts, 2000);
    assert_eq!(member.event_id.as_deref(), Some("$join-p2"));

    // 2. Older join P1 @ 1000 arrives late
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-p1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "P1",
                "avatar_url": "mxc://hs/p1",
            }),
        })
        .await
        .expect("process p1");

    // Older join must be ignored; projection remains P2 @ 2000
    let member_after = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(member_after.membership, "join");
    assert_eq!(member_after.display_name.as_deref(), Some("P2"));
    assert_eq!(member_after.origin_server_ts, 2000);
    assert_eq!(member_after.event_id.as_deref(), Some("$join-p2"));
}

#[tokio::test]
async fn out_of_order_leave_preserves_presentation_when_older_join_arrives() {
    let (processor, store, _media_store) = setup_test_environment().await;
    let room_id = "!room-ordering-2:hs";
    let user_id = "@user2:hs";

    // 1. Join P1 @ 1000
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-p1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "P1",
                "avatar_url": "mxc://hs/p1",
            }),
        })
        .await
        .expect("process p1");

    // 2. Leave @ 3000 (preserves P1)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$leave-p1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 3000,
            content: json!({
                "membership": "leave",
            }),
        })
        .await
        .expect("process leave");

    let member_left = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(member_left.membership, "leave");
    assert_eq!(member_left.display_name.as_deref(), Some("P1"));
    assert_eq!(member_left.origin_server_ts, 3000);

    // 3. Older join P0 @ 2000 arrives late
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-p0".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "P0",
                "avatar_url": "mxc://hs/p0",
            }),
        })
        .await
        .expect("process p0");

    // Must still remain leave + P1 @ 3000
    let member_after_old_join = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(member_after_old_join.membership, "leave");
    assert_eq!(member_after_old_join.display_name.as_deref(), Some("P1"));
    assert_eq!(member_after_old_join.origin_server_ts, 3000);
    assert_eq!(member_after_old_join.event_id.as_deref(), Some("$leave-p1"));
}

#[tokio::test]
async fn out_of_order_ban_preserves_presentation_when_older_join_arrives() {
    let (processor, store, _media_store) = setup_test_environment().await;
    let room_id = "!room-ordering-3:hs";
    let user_id = "@user3:hs";

    // 1. Join P1 @ 1000
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-p1".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "P1",
                "avatar_url": "mxc://hs/p1",
            }),
        })
        .await
        .expect("process p1");

    // 2. Ban @ 3000 (preserves P1)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$ban-p1".to_string(),
            sender: "@admin:hs".to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 3000,
            content: json!({
                "membership": "ban",
            }),
        })
        .await
        .expect("process ban");

    let member_banned = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(member_banned.membership, "ban");
    assert_eq!(member_banned.display_name.as_deref(), Some("P1"));
    assert_eq!(member_banned.origin_server_ts, 3000);

    // 3. Older join P0 @ 2000 arrives late
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-p0".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "P0",
                "avatar_url": "mxc://hs/p0",
            }),
        })
        .await
        .expect("process p0");

    // Must still remain ban + P1 @ 3000
    let member_after_old_join = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(member_after_old_join.membership, "ban");
    assert_eq!(member_after_old_join.display_name.as_deref(), Some("P1"));
    assert_eq!(member_after_old_join.origin_server_ts, 3000);
    assert_eq!(member_after_old_join.event_id.as_deref(), Some("$ban-p1"));
}

#[tokio::test]
async fn out_of_order_newer_join_cannot_be_overwritten_by_older_leave() {
    let (processor, store, _media_store) = setup_test_environment().await;
    let room_id = "!room-ordering-4:hs";
    let user_id = "@user4:hs";

    // 1. Newer join @ 3000
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-3000".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 3000,
            content: json!({
                "membership": "join",
                "displayname": "Newer Join",
            }),
        })
        .await
        .expect("process newer join");

    // 2. Older leave @ 2000 arrives late
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$leave-2000".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "leave",
            }),
        })
        .await
        .expect("process older leave");

    // Projection must remain join
    let member = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(member.membership, "join");
    assert_eq!(member.display_name.as_deref(), Some("Newer Join"));
    assert_eq!(member.origin_server_ts, 3000);
    assert_eq!(member.event_id.as_deref(), Some("$join-3000"));
}

#[tokio::test]
async fn out_of_order_leave_cannot_be_overwritten_by_older_join() {
    let (processor, store, _media_store) = setup_test_environment().await;
    let room_id = "!room-ordering-5:hs";
    let user_id = "@user5:hs";

    // 1. Leave @ 3000
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$leave-3000".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 3000,
            content: json!({
                "membership": "leave",
                "displayname": "Departed",
            }),
        })
        .await
        .expect("process leave");

    // 2. Older join @ 2000 arrives
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-2000".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "Older Join",
            }),
        })
        .await
        .expect("process older join");

    // Projection must remain leave
    let member = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(member.membership, "leave");
    assert_eq!(member.origin_server_ts, 3000);
    assert_eq!(member.event_id.as_deref(), Some("$leave-3000"));
}

#[tokio::test]
async fn equal_timestamps_ordering_is_deterministic_by_event_id() {
    let (processor, store, _media_store) = setup_test_environment().await;
    let room_id = "!room-ordering-6:hs";
    let user_a = "@user6a:hs";
    let user_b = "@user6b:hs";

    // Case 6A: process larger event_id "$b" first, then smaller event_id "$a"
    // $b @ 1000
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$b".to_string(),
            sender: user_a.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_a.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Name B",
            }),
        })
        .await
        .expect("process b");

    // $a @ 1000 arrives late ("$a" < "$b", so $a is older)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$a".to_string(),
            sender: user_a.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_a.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Name A",
            }),
        })
        .await
        .expect("process a");

    let member_a = store.get_member(room_id, user_a).await.unwrap().unwrap();
    assert_eq!(member_a.event_id.as_deref(), Some("$b"));
    assert_eq!(member_a.display_name.as_deref(), Some("Name B"));

    // Case 6B: process smaller event_id "$a" first, then larger event_id "$b"
    // $a @ 1000
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$a".to_string(),
            sender: user_b.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_b.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Name A",
            }),
        })
        .await
        .expect("process a");

    // $b @ 1000 arrives ("$b" > "$a", so $b is newer and updates)
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$b".to_string(),
            sender: user_b.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_b.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Name B",
            }),
        })
        .await
        .expect("process b");

    let member_b = store.get_member(room_id, user_b).await.unwrap().unwrap();
    assert_eq!(member_b.event_id.as_deref(), Some("$b"));
    assert_eq!(member_b.display_name.as_deref(), Some("Name B"));
}

#[tokio::test]
async fn duplicate_member_event_delivery_is_idempotent() {
    let (processor, store, media_store) = setup_test_environment().await;
    let room_id = "!room-ordering-7:hs";
    let user_id = "@user7:hs";
    let mxc_uri = "mxc://hs/user7-avatar";

    let state_event = ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$event-repeat".to_string(),
        sender: user_id.to_string(),
        event_type: "m.room.member".to_string(),
        state_key: user_id.to_string(),
        origin_server_ts: 1500,
        content: json!({
            "membership": "join",
            "displayname": "User Seven",
            "avatar_url": mxc_uri,
        }),
    };

    // First delivery
    processor
        .process_room_state(state_event.clone())
        .await
        .expect("first delivery");
    let member_1 = store.get_member(room_id, user_id).await.unwrap().unwrap();

    // Second delivery of identical event
    processor
        .process_room_state(state_event)
        .await
        .expect("second delivery");
    let member_2 = store.get_member(room_id, user_id).await.unwrap().unwrap();

    assert_eq!(member_1, member_2);
    assert_eq!(member_2.display_name.as_deref(), Some("User Seven"));
    assert_eq!(member_2.avatar_url.as_deref(), Some(mxc_uri));
    assert_eq!(member_2.origin_server_ts, 1500);
    assert_eq!(member_2.event_id.as_deref(), Some("$event-repeat"));

    // Media reference store must have at most 1 reference (no duplicates created)
    let site_id = SiteId::from("example.com");
    let ref_found = media_store.find_reference(&site_id, mxc_uri).await.unwrap();
    assert!(ref_found.is_none()); // because no local upload record exists, none created
}

#[tokio::test]
async fn ignored_older_event_does_not_create_media_reference() {
    let (processor, store, media_store) = setup_test_environment().await;
    let room_id = "!room-ordering-8:hs";
    let user_id = "@user8:hs";
    let site_id = "example.com";
    let old_mxc = "mxc://hs/old-avatar";
    let site = SiteId::from(site_id);
    let page_slug = PageSlug::from("page-8");
    store
        .register_room(room_id, &site, &page_slug)
        .await
        .expect("register room");

    // Create an authoritative local upload record for the old avatar
    store
        .record_media_upload(old_mxc, "author-8", site_id, None)
        .await
        .expect("record media upload");

    // 1. Newer join @ 2000 without avatar
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-newer".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "Newer User",
            }),
        })
        .await
        .expect("process newer join");

    // 2. Older join @ 1000 with old_mxc arrives late
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$join-older".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "Older User",
                "avatar_url": old_mxc,
            }),
        })
        .await
        .expect("process older join");

    // Since older event was ignored, no MediaReference mapping should have been created!
    let mapping = media_store.find_reference(&site, old_mxc).await.unwrap();
    assert!(
        mapping.is_none(),
        "ignored older event must not create MediaReference"
    );
}

#[tokio::test]
async fn projection_replay_older_to_newer_vs_newer_to_older_converges() {
    let (processor, store, _media_store) = setup_test_environment().await;
    let room_id_forward = "!room-forward:hs";
    let user_forward = "@user-forward:hs";
    let room_id_reverse = "!room-reverse:hs";
    let user_reverse = "@user-reverse:hs";

    let events_forward = [
        ParsedRoomState {
            room_id: room_id_forward.to_string(),
            event_id: "$e1".to_string(),
            sender: user_forward.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_forward.to_string(),
            origin_server_ts: 1000,
            content: json!({ "membership": "join", "displayname": "Alice 1" }),
        },
        ParsedRoomState {
            room_id: room_id_forward.to_string(),
            event_id: "$e2".to_string(),
            sender: user_forward.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_forward.to_string(),
            origin_server_ts: 2000,
            content: json!({ "membership": "join", "displayname": "Alice 2" }),
        },
        ParsedRoomState {
            room_id: room_id_forward.to_string(),
            event_id: "$e3".to_string(),
            sender: user_forward.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_forward.to_string(),
            origin_server_ts: 3000,
            content: json!({ "membership": "leave" }),
        },
    ];

    let events_reverse = [
        ParsedRoomState {
            room_id: room_id_reverse.to_string(),
            event_id: "$e3".to_string(),
            sender: user_reverse.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_reverse.to_string(),
            origin_server_ts: 3000,
            content: json!({ "membership": "leave", "displayname": "Alice 2" }),
        },
        ParsedRoomState {
            room_id: room_id_reverse.to_string(),
            event_id: "$e2".to_string(),
            sender: user_reverse.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_reverse.to_string(),
            origin_server_ts: 2000,
            content: json!({ "membership": "join", "displayname": "Alice 2" }),
        },
        ParsedRoomState {
            room_id: room_id_reverse.to_string(),
            event_id: "$e1".to_string(),
            sender: user_reverse.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_reverse.to_string(),
            origin_server_ts: 1000,
            content: json!({ "membership": "join", "displayname": "Alice 1" }),
        },
    ];

    // Replay forward
    for event in events_forward {
        processor.process_room_state(event).await.unwrap();
    }
    let state_forward = store
        .get_member(room_id_forward, user_forward)
        .await
        .unwrap()
        .unwrap();

    // Replay reverse
    for event in events_reverse {
        processor.process_room_state(event).await.unwrap();
    }
    let state_reverse = store
        .get_member(room_id_reverse, user_reverse)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(state_forward.membership, state_reverse.membership);
    assert_eq!(state_forward.display_name, state_reverse.display_name);
    assert_eq!(
        state_forward.origin_server_ts,
        state_reverse.origin_server_ts
    );
    assert_eq!(state_forward.event_id, state_reverse.event_id);
}

#[tokio::test]
async fn monotonic_projection_ordering_and_rebuild_via_event_processor() {
    let (processor, store, _media_store) = setup_test_environment().await;
    let room_id = "!room-monotonic:hs";
    let user_id = "@monotonic_user:hs";

    // 1. Initial join event at ts 2000 with event_id "$event_b"
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$event_b".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "User B",
                "avatar_url": "mxc://hs/avatar-b",
            }),
        })
        .await
        .expect("process initial join");

    let initial = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(initial.display_name.as_deref(), Some("User B"));
    assert_eq!(initial.origin_server_ts, 2000);
    assert_eq!(initial.event_id.as_deref(), Some("$event_b"));

    // 2. Incoming event at the same timestamp (2000) with lexicographically smaller event_id "$event_a" is rejected
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$event_a".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "User A",
                "avatar_url": "mxc://hs/avatar-a",
            }),
        })
        .await
        .expect("process smaller same-ts event");

    let after_smaller = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(after_smaller.display_name.as_deref(), Some("User B"));
    assert_eq!(after_smaller.event_id.as_deref(), Some("$event_b"));

    // 3. Incoming event at the same timestamp (2000) with lexicographically larger event_id "$event_c" is accepted
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$event_c".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "User C",
                "avatar_url": "mxc://hs/avatar-c",
            }),
        })
        .await
        .expect("process larger same-ts event");

    let after_larger = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(after_larger.display_name.as_deref(), Some("User C"));
    assert_eq!(after_larger.event_id.as_deref(), Some("$event_c"));

    // 4. Older event at timestamp 1000 is rejected
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$event_old".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 1000,
            content: json!({
                "membership": "join",
                "displayname": "User Old",
                "avatar_url": "mxc://hs/avatar-old",
            }),
        })
        .await
        .expect("process older event");

    let after_older = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(after_older.display_name.as_deref(), Some("User C"));
    assert_eq!(after_older.origin_server_ts, 2000);
    assert_eq!(after_older.event_id.as_deref(), Some("$event_c"));

    // 5. Projection reset / rebuild: wipe room_members projection
    store
        .delete_member(room_id, user_id)
        .await
        .expect("wipe member projection");
    assert!(store.get_member(room_id, user_id).await.unwrap().is_none());

    // Replay canonical event through processor
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$event_c".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 2000,
            content: json!({
                "membership": "join",
                "displayname": "User C",
                "avatar_url": "mxc://hs/avatar-c",
            }),
        })
        .await
        .expect("replay event");

    let rebuilt = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(rebuilt.display_name.as_deref(), Some("User C"));
    assert_eq!(rebuilt.origin_server_ts, 2000);
    assert_eq!(rebuilt.event_id.as_deref(), Some("$event_c"));

    // 6. Newer event at timestamp 3000 updates projection
    processor
        .process_room_state(ParsedRoomState {
            room_id: room_id.to_string(),
            event_id: "$newer_event".to_string(),
            sender: user_id.to_string(),
            event_type: "m.room.member".to_string(),
            state_key: user_id.to_string(),
            origin_server_ts: 3000,
            content: json!({
                "membership": "join",
                "displayname": "Updated User",
                "avatar_url": "mxc://hs/updated",
            }),
        })
        .await
        .expect("process newer event");

    let member_after_newer = store.get_member(room_id, user_id).await.unwrap().unwrap();
    assert_eq!(
        member_after_newer.display_name.as_deref(),
        Some("Updated User")
    );
    assert_eq!(
        member_after_newer.avatar_url.as_deref(),
        Some("mxc://hs/updated")
    );
    assert_eq!(member_after_newer.event_id.as_deref(), Some("$newer_event"));
    assert_eq!(member_after_newer.origin_server_ts, 3000);
}

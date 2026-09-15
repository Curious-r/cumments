//! Integration tests for Historical Room-State Resolution (DAG context).
//!
//! Verifies:
//! 1. Historical snapshot is immutable when author profile changes later.
//! 2. Canonical history P1 -> E -> P2 resolves E as P1 under all ingestion permutations (P1->E->P2, P2->E->P1, E->P2->P1).
//! 3. Leave after message preserves historical snapshot.
//! 4. Leave before message resolves effective DAG state without falling back to old presentation or current room presentation.
//! 5. Lifecycle (join P1 -> join P2 -> message at P2 -> leave -> join P3) maintains P2 snapshot while live view shows P3.
//! 6. Resolver failure fails message processing explicitly without fallback to room_members.
//! 7. Missing resolver fails message processing explicitly.
//! 8. Logging driver returns unsupported error and message processing fails explicitly.
//! 9. Valid "no usable presentation" is distinct from resolver failure.
//! 10. Historical media references are derived deterministically from the event's avatar MXC for existing references, local uploads, and unknown avatars alike.
//! 11. Rebuild determinism: chronological vs reverse replay produces identical stored snapshots.

use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::broadcast;

use cumments_core::media_reference::MediaReference;
use cumments_core::models::{Content, PageSlug, RoomIdentity, SiteId, TextContent, TextStyle};
use cumments_core::ports::{
    HistoricalRoomStateResolver, MediaReferenceResolver, MediaReferenceStore, MessageStore,
    RegistryStore, RoomStore, SiteStore,
};
use cumments_matrix::LoggingMatrixDriver;
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::{ParsedRoomMessage, ParsedRoomState};
use cumments_store::DbStore;
use cumments_store::entities::messages;
use cumments_test_utils::MockHomeserver;
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

fn create_processor_with_resolver(
    store: Arc<DbStore>,
    resolver: Option<Arc<dyn HistoricalRoomStateResolver>>,
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
        driver: None,
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(Notify::new()),
        projection_notify: Arc::new(Notify::new()),
        server_name: Some("hs".to_string()),
        media_reference_store: Some(store.clone()),
        historical_state_resolver: resolver,
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
    let homeserver = MockHomeserver::start().await;

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

    // Mount context response at $msg-1: state at $msg-1 is P1
    homeserver
        .mount_context(
            room_id,
            event_id,
            MockHomeserver::make_message_event(room_id, event_id, user_id, "Hello world", 1050),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "join",
                Some("Alice P1"),
                Some("mxc://hs/alice1"),
            )],
        )
        .await;

    let processor =
        create_processor_with_resolver(store.clone(), Some(homeserver.historical_resolver()));

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

    // 2. Process message at $msg-1 (resolves P1 through homeserver /context)
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

    // 3. Update profile to P2 later
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

    // Check stored raw message row still holds immutable P1
    let raw = get_raw_message(&store, event_id)
        .await
        .expect("raw message exists");
    assert_eq!(raw.author_display_name.as_deref(), Some("Alice P1"));
    assert_eq!(raw.author_avatar_url.as_deref(), Some("mxc://hs/alice1"));

    // Check live message view enriches with current room_members (P2)
    let page = store
        .get_messages(&site_id, &page_slug, 10, 0, None)
        .await
        .expect("get messages");
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].author.display_name.as_deref(),
        Some("Alice P2")
    );
    assert_eq!(
        page.items[0].author.avatar_url.as_deref(),
        Some("mxc://hs/alice2")
    );
}

#[tokio::test]
async fn canonical_history_resolves_p1_under_all_ingestion_permutations() {
    let permutations = [
        ("p1_e_p2", vec!["p1", "e", "p2"]),
        ("p2_e_p1", vec!["p2", "e", "p1"]),
        ("e_p2_p1", vec!["e", "p2", "p1"]),
    ];

    for (name, order) in permutations {
        let db_url = test_db_url(&format!("permutation_{name}"));
        let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
        let homeserver = MockHomeserver::start().await;

        let site_id = SiteId::from("test-site");
        let page_slug = PageSlug::from("test-page");
        let room_id = "!room:hs";
        let user_id = "@alice:hs";
        let event_id = "$msg-perm";

        store
            .ensure_site_exists(site_id.as_str(), "!space:hs")
            .await
            .expect("ensure site");
        store
            .register_room(room_id, &site_id, &page_slug)
            .await
            .expect("register room");

        // The canonical state at message E in the room DAG is P1.
        homeserver
            .mount_context(
                room_id,
                event_id,
                MockHomeserver::make_message_event(room_id, event_id, user_id, "Permuted", 1500),
                vec![MockHomeserver::make_member_state_event(
                    user_id,
                    "join",
                    Some("Alice P1"),
                    Some("mxc://hs/alice1"),
                )],
            )
            .await;

        let processor =
            create_processor_with_resolver(store.clone(), Some(homeserver.historical_resolver()));

        for step in order {
            match step {
                "p1" => {
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
                        .expect("process p1");
                }
                "e" => {
                    processor
                        .process_room_message(make_room_message(
                            room_id,
                            event_id,
                            user_id,
                            "Permuted",
                            1500,
                            site_id.as_str(),
                            page_slug.as_str(),
                        ))
                        .await
                        .expect("process e");
                }
                "p2" => {
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
                        .expect("process p2");
                }
                _ => unreachable!(),
            }
        }

        // Under all ingestion permutations, event E must resolve to historical P1 snapshot!
        let raw = get_raw_message(&store, event_id)
            .await
            .expect("raw message exists");
        assert_eq!(
            raw.author_display_name.as_deref(),
            Some("Alice P1"),
            "failed for order {:?}",
            step_order_desc(name)
        );
        assert_eq!(
            raw.author_avatar_url.as_deref(),
            Some("mxc://hs/alice1"),
            "failed for order {:?}",
            step_order_desc(name)
        );

        // And current room presentation in room_members reflects P2 (latest timestamp)
        let member = store
            .get_member(room_id, user_id)
            .await
            .expect("get member")
            .expect("member exists");
        assert_eq!(member.display_name.as_deref(), Some("Alice P2"));
    }
}

fn step_order_desc(name: &str) -> &'static str {
    match name {
        "p1_e_p2" => "P1 -> E -> P2",
        "p2_e_p1" => "P2 -> E -> P1",
        "e_p2_p1" => "E -> P2 -> P1",
        _ => "unknown",
    }
}

#[tokio::test]
async fn author_leave_after_message_preserves_historical_snapshot() {
    let db_url = test_db_url("leave_after_message");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let homeserver = MockHomeserver::start().await;

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

    homeserver
        .mount_context(
            room_id,
            event_id,
            MockHomeserver::make_message_event(room_id, event_id, user_id, "Goodbye", 1050),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "join",
                Some("Alice P1"),
                Some("mxc://hs/alice1"),
            )],
        )
        .await;

    let processor =
        create_processor_with_resolver(store.clone(), Some(homeserver.historical_resolver()));

    // 1. Join at P1
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
        .expect("process join");

    // 2. Post message at 1050
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Goodbye",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process message");

    // 3. Leave at 2000
    processor
        .process_room_state(make_member_state(
            room_id,
            "$leave-1",
            user_id,
            "leave",
            Some("Alice P1"),
            Some("mxc://hs/alice1"),
            2000,
        ))
        .await
        .expect("process leave");

    // Stored message keeps P1 snapshot
    let raw = get_raw_message(&store, event_id)
        .await
        .expect("raw message exists");
    assert_eq!(raw.author_display_name.as_deref(), Some("Alice P1"));
    assert_eq!(raw.author_avatar_url.as_deref(), Some("mxc://hs/alice1"));

    // Live query still enriches with P1 under Model A leave semantics
    let page = store
        .get_messages(&site_id, &page_slug, 10, 0, None)
        .await
        .expect("get messages");
    assert_eq!(
        page.items[0].author.display_name.as_deref(),
        Some("Alice P1")
    );
}

#[tokio::test]
async fn leave_before_message_resolves_effective_dag_state() {
    let db_url = test_db_url("leave_before_message");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let homeserver = MockHomeserver::start().await;

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-after-leave";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    // Context at event E shows user in "leave" state with profile "Alice Departed"
    homeserver
        .mount_context(
            room_id,
            event_id,
            MockHomeserver::make_message_event(room_id, event_id, user_id, "Ghost message", 2500),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "leave",
                Some("Alice Departed"),
                Some("mxc://hs/departed"),
            )],
        )
        .await;

    let processor =
        create_processor_with_resolver(store.clone(), Some(homeserver.historical_resolver()));

    // 1. Initial join at P1
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
        .expect("process join");

    // 2. Leave at 2000 with "Alice Departed"
    processor
        .process_room_state(make_member_state(
            room_id,
            "$leave-1",
            user_id,
            "leave",
            Some("Alice Departed"),
            Some("mxc://hs/departed"),
            2000,
        ))
        .await
        .expect("process leave");

    // 3. Process message at 2500
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Ghost message",
            2500,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process message");

    // The message must resolve to the DAG state effective at E ("Alice Departed"),
    // not fall back to joined state "Alice P1".
    let raw = get_raw_message(&store, event_id)
        .await
        .expect("raw message exists");
    assert_eq!(raw.author_display_name.as_deref(), Some("Alice Departed"));
    assert_eq!(raw.author_avatar_url.as_deref(), Some("mxc://hs/departed"));
}

#[tokio::test]
async fn lifecycle_rejoin_maintains_message_snapshot_while_live_shows_new_profile() {
    let db_url = test_db_url("lifecycle_rejoin");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let homeserver = MockHomeserver::start().await;

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

    // Context at $msg-at-p2 is P2
    homeserver
        .mount_context(
            room_id,
            event_id,
            MockHomeserver::make_message_event(room_id, event_id, user_id, "Message at P2", 2500),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "join",
                Some("Alice P2"),
                Some("mxc://hs/alice2"),
            )],
        )
        .await;

    let processor =
        create_processor_with_resolver(store.clone(), Some(homeserver.historical_resolver()));

    // 1. Join P1 (1000)
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
        .expect("process join 1");

    // 2. Update to P2 (2000)
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
        .expect("process join 2");

    // 3. Post message at 2500 (effective state is P2)
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Message at P2",
            2500,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process message");

    // 4. Leave at 3000
    processor
        .process_room_state(make_member_state(
            room_id,
            "$leave-1",
            user_id,
            "leave",
            Some("Alice P2"),
            Some("mxc://hs/alice2"),
            3000,
        ))
        .await
        .expect("process leave");

    // 5. Rejoin at 4000 as P3
    processor
        .process_room_state(make_member_state(
            room_id,
            "$member-3",
            user_id,
            "join",
            Some("Alice P3"),
            Some("mxc://hs/alice3"),
            4000,
        ))
        .await
        .expect("process rejoin P3");

    // Historical raw snapshot must remain P2
    let raw = get_raw_message(&store, event_id)
        .await
        .expect("raw message exists");
    assert_eq!(raw.author_display_name.as_deref(), Some("Alice P2"));
    assert_eq!(raw.author_avatar_url.as_deref(), Some("mxc://hs/alice2"));

    // Live view query enriches with P3 (latest room state)
    let page = store
        .get_messages(&site_id, &page_slug, 10, 0, None)
        .await
        .expect("get messages");
    assert_eq!(
        page.items[0].author.display_name.as_deref(),
        Some("Alice P3")
    );
    assert_eq!(
        page.items[0].author.avatar_url.as_deref(),
        Some("mxc://hs/alice3")
    );
}

#[tokio::test]
async fn resolver_failure_fails_processing_explicitly_without_room_members_fallback() {
    let db_url = test_db_url("resolver_failure");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let homeserver = MockHomeserver::start().await;

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

    // Configure homeserver to return 500 error on /context
    homeserver
        .mount_context_response(
            room_id,
            event_id,
            500,
            json!({ "errcode": "M_UNKNOWN", "error": "Internal server error" }),
        )
        .await;

    let processor =
        create_processor_with_resolver(store.clone(), Some(homeserver.historical_resolver()));

    // Initial member state P1 exists in room_members
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
        .expect("process join");

    // Process message should FAIL explicitly and NOT fall back to room_members P1!
    let err = processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Should fail",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect_err("message processing must fail on resolver error");

    assert!(err.to_string().contains("500") || err.to_string().contains("failed"));

    // Message must NOT be saved in database
    let raw = get_raw_message(&store, event_id).await;
    assert!(
        raw.is_none(),
        "failed message must not be persisted to messages table"
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

    let processor = create_processor_with_resolver(store.clone(), None);

    let err = processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "No resolver",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect_err("message processing must fail when resolver is missing");

    assert!(
        err.to_string()
            .contains("HistoricalRoomStateResolver is unavailable")
    );
}

#[tokio::test]
async fn unsupported_logging_driver_fails_explicitly() {
    let db_url = test_db_url("logging_driver_unsupported");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-logging";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    let logging_resolver = Arc::new(LoggingMatrixDriver) as Arc<dyn HistoricalRoomStateResolver>;
    let processor = create_processor_with_resolver(store.clone(), Some(logging_resolver));

    let err = processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Logging test",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect_err("message processing must fail when resolver is LoggingMatrixDriver");

    assert!(
        err.to_string()
            .contains("not supported by LoggingMatrixDriver")
    );
    assert!(get_raw_message(&store, event_id).await.is_none());
}

#[tokio::test]
async fn no_usable_presentation_is_distinct_from_resolver_failure() {
    let db_url = test_db_url("no_usable_presentation");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let homeserver = MockHomeserver::start().await;

    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";
    let event_id = "$msg-no-pres";

    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");
    store
        .register_room(room_id, &site_id, &page_slug)
        .await
        .expect("register room");

    // Member state has membership: "join", but no displayname and no avatar_url
    homeserver
        .mount_context(
            room_id,
            event_id,
            MockHomeserver::make_message_event(room_id, event_id, user_id, "Bare member", 1050),
            vec![MockHomeserver::make_member_state_event(
                user_id, "join", None, None,
            )],
        )
        .await;

    let processor =
        create_processor_with_resolver(store.clone(), Some(homeserver.historical_resolver()));

    // Message processing must succeed with None profile (valid absence)
    processor
        .process_room_message(make_room_message(
            room_id,
            event_id,
            user_id,
            "Bare member",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process room message succeeds with None presentation");

    let raw = get_raw_message(&store, event_id)
        .await
        .expect("raw message exists");
    assert_eq!(raw.author_display_name, None);
    assert_eq!(raw.author_avatar_url, None);
    assert_eq!(raw.author_media_reference, None);
}

#[tokio::test]
async fn durable_media_reference_resolution_and_deterministic_unknown_avatar() {
    let db_url = test_db_url("media_ref_resolution");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let homeserver = MockHomeserver::start().await;

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

    let processor =
        create_processor_with_resolver(store.clone(), Some(homeserver.historical_resolver()));

    // 1. Message 1: Existing MediaReference in media_references table
    let existing_ref = store
        .get_or_create_reference(&site_id, "mxc://hs/existing-avatar")
        .await
        .expect("create reference");

    homeserver
        .mount_context(
            room_id,
            "$msg-existing-ref",
            MockHomeserver::make_message_event(
                room_id,
                "$msg-existing-ref",
                user_id,
                "Msg 1",
                1000,
            ),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "join",
                Some("Alice Existing"),
                Some("mxc://hs/existing-avatar"),
            )],
        )
        .await;

    processor
        .process_room_message(make_room_message(
            room_id,
            "$msg-existing-ref",
            user_id,
            "Msg 1",
            1000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process msg 1");

    let raw1 = get_raw_message(&store, "$msg-existing-ref").await.unwrap();
    assert_eq!(
        raw1.author_media_reference.as_deref(),
        Some(existing_ref.as_str())
    );

    // 2. Message 2: Authoritative local upload in media_uploads table
    store
        .record_media_upload(
            "mxc://hs/uploaded-avatar",
            "author-pubkey-1",
            site_id.as_str(),
            None,
        )
        .await
        .expect("record upload");

    homeserver
        .mount_context(
            room_id,
            "$msg-uploaded-ref",
            MockHomeserver::make_message_event(
                room_id,
                "$msg-uploaded-ref",
                user_id,
                "Msg 2",
                2000,
            ),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "join",
                Some("Alice Uploaded"),
                Some("mxc://hs/uploaded-avatar"),
            )],
        )
        .await;

    processor
        .process_room_message(make_room_message(
            room_id,
            "$msg-uploaded-ref",
            user_id,
            "Msg 2",
            2000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process msg 2");

    let raw2 = get_raw_message(&store, "$msg-uploaded-ref").await.unwrap();
    assert!(raw2.author_media_reference.is_some());
    let ref2 = raw2.author_media_reference.unwrap();
    assert!(ref2.starts_with("cumments-media:"));

    // 3. Message 3: Unknown provenance MXC (no local upload record, not in media_references)
    homeserver
        .mount_context(
            room_id,
            "$msg-unknown-ref",
            MockHomeserver::make_message_event(room_id, "$msg-unknown-ref", user_id, "Msg 3", 3000),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "join",
                Some("Alice Unknown"),
                Some("mxc://hs/speculative-external"),
            )],
        )
        .await;

    processor
        .process_room_message(make_room_message(
            room_id,
            "$msg-unknown-ref",
            user_id,
            "Msg 3",
            3000,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .expect("process msg 3");

    let raw3 = get_raw_message(&store, "$msg-unknown-ref").await.unwrap();
    // The raw compatibility MXC is kept, and the media reference is derived
    // deterministically from the event's avatar even though no lookup mapping
    // existed. The mapping is materialized so the persisted reference resolves.
    assert_eq!(
        raw3.author_avatar_url.as_deref(),
        Some("mxc://hs/speculative-external")
    );
    let expected3 = MediaReference::from_media(&site_id, "mxc://hs/speculative-external");
    assert_eq!(
        raw3.author_media_reference.as_deref(),
        Some(expected3.as_str())
    );
    assert_eq!(
        store
            .find_reference(&site_id, "mxc://hs/speculative-external")
            .await
            .unwrap(),
        Some(expected3.clone())
    );
    assert_eq!(
        store
            .resolve_mxc(&site_id, &expected3)
            .await
            .unwrap()
            .as_deref(),
        Some("mxc://hs/speculative-external")
    );
    assert!(
        store
            .get_record(&site_id, &expected3)
            .await
            .unwrap()
            .is_some(),
        "the lookup mapping must be materialized"
    );
}

#[tokio::test]
async fn rebuild_determinism_chronological_vs_reverse_order() {
    let site_id = SiteId::from("test-site");
    let page_slug = PageSlug::from("test-page");
    let room_id = "!room:hs";
    let user_id = "@alice:hs";

    let homeserver = MockHomeserver::start().await;

    // Messages at 1050 (P1) and 2050 (P2)
    homeserver
        .mount_context(
            room_id,
            "$msg-rebuild-1",
            MockHomeserver::make_message_event(room_id, "$msg-rebuild-1", user_id, "Msg 1", 1050),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "join",
                Some("Alice P1"),
                Some("mxc://hs/alice1"),
            )],
        )
        .await;

    homeserver
        .mount_context(
            room_id,
            "$msg-rebuild-2",
            MockHomeserver::make_message_event(room_id, "$msg-rebuild-2", user_id, "Msg 2", 2050),
            vec![MockHomeserver::make_member_state_event(
                user_id,
                "join",
                Some("Alice P2"),
                Some("mxc://hs/alice2"),
            )],
        )
        .await;

    // Database 1: chronological replay
    let db1_url = test_db_url("rebuild_chrono");
    let store1 = Arc::new(DbStore::connect(&db1_url).await.expect("connect db1"));
    store1
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();
    store1
        .register_room(room_id, &site_id, &page_slug)
        .await
        .unwrap();
    let processor1 =
        create_processor_with_resolver(store1.clone(), Some(homeserver.historical_resolver()));

    processor1
        .process_room_state(make_member_state(
            room_id,
            "$m-1",
            user_id,
            "join",
            Some("Alice P1"),
            Some("mxc://hs/alice1"),
            1000,
        ))
        .await
        .unwrap();
    processor1
        .process_room_message(make_room_message(
            room_id,
            "$msg-rebuild-1",
            user_id,
            "Msg 1",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .unwrap();
    processor1
        .process_room_state(make_member_state(
            room_id,
            "$m-2",
            user_id,
            "join",
            Some("Alice P2"),
            Some("mxc://hs/alice2"),
            2000,
        ))
        .await
        .unwrap();
    processor1
        .process_room_message(make_room_message(
            room_id,
            "$msg-rebuild-2",
            user_id,
            "Msg 2",
            2050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .unwrap();

    // Database 2: reverse replay
    let db2_url = test_db_url("rebuild_reverse");
    let store2 = Arc::new(DbStore::connect(&db2_url).await.expect("connect db2"));
    store2
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();
    store2
        .register_room(room_id, &site_id, &page_slug)
        .await
        .unwrap();
    let processor2 =
        create_processor_with_resolver(store2.clone(), Some(homeserver.historical_resolver()));

    processor2
        .process_room_message(make_room_message(
            room_id,
            "$msg-rebuild-2",
            user_id,
            "Msg 2",
            2050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .unwrap();
    processor2
        .process_room_state(make_member_state(
            room_id,
            "$m-2",
            user_id,
            "join",
            Some("Alice P2"),
            Some("mxc://hs/alice2"),
            2000,
        ))
        .await
        .unwrap();
    processor2
        .process_room_message(make_room_message(
            room_id,
            "$msg-rebuild-1",
            user_id,
            "Msg 1",
            1050,
            site_id.as_str(),
            page_slug.as_str(),
        ))
        .await
        .unwrap();
    processor2
        .process_room_state(make_member_state(
            room_id,
            "$m-1",
            user_id,
            "join",
            Some("Alice P1"),
            Some("mxc://hs/alice1"),
            1000,
        ))
        .await
        .unwrap();

    // Verify both databases have identical stored snapshots for both messages!
    let msg1_db1 = get_raw_message(&store1, "$msg-rebuild-1").await.unwrap();
    let msg1_db2 = get_raw_message(&store2, "$msg-rebuild-1").await.unwrap();
    assert_eq!(msg1_db1.author_display_name, msg1_db2.author_display_name);
    assert_eq!(msg1_db1.author_avatar_url, msg1_db2.author_avatar_url);
    assert_eq!(msg1_db1.author_display_name.as_deref(), Some("Alice P1"));

    let msg2_db1 = get_raw_message(&store1, "$msg-rebuild-2").await.unwrap();
    let msg2_db2 = get_raw_message(&store2, "$msg-rebuild-2").await.unwrap();
    assert_eq!(msg2_db1.author_display_name, msg2_db2.author_display_name);
    assert_eq!(msg2_db1.author_avatar_url, msg2_db2.author_avatar_url);
    assert_eq!(msg2_db1.author_display_name.as_deref(), Some("Alice P2"));
}

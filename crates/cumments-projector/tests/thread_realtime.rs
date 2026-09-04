//! Thread realtime semantics: member creation and redaction publish an
//! authoritative ThreadSummary snapshot for the root through the existing
//! `MessageAnnotationsChanged` event, derived from committed read-model state.

use cumments_core::models::{Content, RoomIdentity, TextContent, TextStyle, ThreadSummary};
use cumments_core::ports::RegistryStore;
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::{ParsedRoomMessage, ParsedRoomRedaction};
use cumments_store::DbStore;
use std::sync::Arc;
use tokio::sync::broadcast;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-thread-realtime-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn identity() -> RoomIdentity {
    RoomIdentity {
        site_id: "my-blog".to_string(),
        page_slug: "hello".to_string(),
    }
}

async fn processor(store: Arc<DbStore>) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(EventProcessorDeps {
        site_store: store.clone() as Arc<dyn cumments_core::ports::SiteStore>,
        registry_store: store.clone() as Arc<dyn cumments_core::ports::RegistryStore>,
        message_store: store.clone() as Arc<dyn cumments_core::ports::MessageStore>,
        room_store: store.clone() as Arc<dyn cumments_core::ports::RoomStore>,
        governance_store: store.clone() as Arc<dyn cumments_core::ports::GovernanceStore>,
        sticker_pack_store: store.clone() as Arc<dyn cumments_core::ports::StickerPackStore>,
        projection_repair_store: store.clone()
            as Arc<dyn cumments_core::ports::ProjectionRepairStore>,
        role_claim_store: store.clone() as Arc<dyn cumments_core::ports::RoleClaimStore>,
        submission_store: store.clone() as Arc<dyn cumments_core::ports::SubmissionStore>,
        audit_store: store.clone() as Arc<dyn cumments_core::ports::CommandAuditStore>,
        site_auth_store: store.clone() as Arc<dyn cumments_core::ports::SiteAuthStore>,
        site_auth_policy: std::sync::Arc::new(cumments_core::site_auth::SiteAuthPolicy {
            verification: cumments_core::site_auth::SiteVerificationPolicy::Optional,
            sites: Default::default(),
        }),
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

fn message(
    event_id: &str,
    thread_root: Option<&str>,
    reply_to: Option<&str>,
    ts: i64,
) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: "!room:hs".to_string(),
        event_id: event_id.to_string(),
        event_type: "m.room.message".to_string(),
        sender: "@alice:hs".to_string(),
        content: Content::Text(TextContent {
            body: event_id.to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        author_public_key: None,
        author_signature: None,
        author_challenge: None,
        is_virtual_user_sender: false,
        submission_id: None,
        reply_to: reply_to.map(str::to_string),
        thread_root: thread_root.map(str::to_string),
        origin_server_ts: ts,
        relates_to: None,
        room_identity: Some(identity()),
        raw_content: serde_json::Value::Null,
    }
}

fn redaction(event_id: &str, ts: i64, redacts: &str) -> ParsedRoomRedaction {
    ParsedRoomRedaction {
        room_id: "!room:hs".to_string(),
        event_id: event_id.to_string(),
        sender: Some("@moderator:hs".to_string()),
        origin_server_ts: ts,
        redacts: Some(redacts.to_string()),
        proof: None,
        submission_id: None,
        room_identity: Some(identity()),
    }
}

async fn setup(name: &str) -> (Arc<DbStore>, EventProcessor) {
    let store = Arc::new(
        DbStore::connect(&test_db_url(name))
            .await
            .expect("connect db"),
    );
    store
        .register_room(
            "!room:hs",
            &cumments_core::models::SiteId::from("my-blog"),
            &cumments_core::models::PageSlug::from("hello"),
        )
        .await
        .expect("register room");
    let processor = processor(store.clone()).await;
    (store, processor)
}

#[tokio::test]
async fn thread_member_creation_emits_root_summary_snapshot() {
    let (_store, processor) = setup("member-create").await;

    // The root itself is an ordinary message: no annotation snapshot.
    processor.start_event_capture().await;
    processor
        .process_room_message(message("$root:hs", None, None, 100))
        .await
        .expect("project root");
    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 1, "root creation emits only MessageCreated");
    assert!(matches!(
        &captured[0],
        cumments_core::projector_events::ProjectorEvent::MessageCreated { message, .. }
            if message.event_id == "$root:hs"
    ));

    // A member joining the Thread emits its own MessageCreated plus an
    // authoritative summary snapshot for the root.
    processor.start_event_capture().await;
    processor
        .process_room_message(message("$a:hs", Some("$root:hs"), Some("$root:hs"), 200))
        .await
        .expect("project member a");
    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 2);
    assert!(matches!(
        &captured[0],
        cumments_core::projector_events::ProjectorEvent::MessageCreated { message, .. }
            if message.event_id == "$a:hs"
    ));
    match &captured[1] {
        cumments_core::projector_events::ProjectorEvent::MessageAnnotationsChanged {
            message,
            ..
        } => {
            assert_eq!(message.event_id, "$root:hs");
            assert_eq!(
                message.thread_summary,
                Some(ThreadSummary {
                    num_replies: 1,
                    latest_reply: Some("$a:hs".to_string())
                }),
                "the annotation payload is the current committed summary, not a delta"
            );
        }
        other => panic!("expected MessageAnnotationsChanged, got {other:?}"),
    }

    // A second member with a later timestamp moves the snapshot's latest.
    processor.start_event_capture().await;
    processor
        .process_room_message(message("$b:hs", Some("$root:hs"), None, 300))
        .await
        .expect("project member b");
    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 2);
    match &captured[1] {
        cumments_core::projector_events::ProjectorEvent::MessageAnnotationsChanged {
            message,
            ..
        } => {
            assert_eq!(message.event_id, "$root:hs");
            assert_eq!(
                message.thread_summary,
                Some(ThreadSummary {
                    num_replies: 2,
                    latest_reply: Some("$b:hs".to_string())
                })
            );
        }
        other => panic!("expected MessageAnnotationsChanged, got {other:?}"),
    }
}

#[tokio::test]
async fn thread_member_redaction_emits_root_summary_snapshot() {
    let (_store, processor) = setup("member-redact").await;

    processor
        .process_room_message(message("$root:hs", None, None, 100))
        .await
        .expect("project root");
    processor
        .process_room_message(message("$a:hs", Some("$root:hs"), Some("$root:hs"), 200))
        .await
        .expect("project member a");
    processor
        .process_room_message(message("$b:hs", Some("$root:hs"), None, 300))
        .await
        .expect("project member b");

    // Redacting the latest member emits its deletion plus a refreshed root
    // snapshot; a stale latest_reply can never survive.
    processor.start_event_capture().await;
    processor
        .process_room_redaction(redaction("$redact:hs", 400, "$b:hs"))
        .await
        .expect("redact member b");
    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 2);
    assert!(matches!(
        &captured[0],
        cumments_core::projector_events::ProjectorEvent::MessageDeleted { event_id, .. }
            if event_id == "$b:hs"
    ));
    match &captured[1] {
        cumments_core::projector_events::ProjectorEvent::MessageAnnotationsChanged {
            message,
            ..
        } => {
            assert_eq!(message.event_id, "$root:hs");
            assert_eq!(
                message.thread_summary,
                Some(ThreadSummary {
                    num_replies: 1,
                    latest_reply: Some("$a:hs".to_string())
                })
            );
        }
        other => panic!("expected MessageAnnotationsChanged, got {other:?}"),
    }

    // Redacting the root changes no membership: the deletion alone is enough.
    processor.start_event_capture().await;
    processor
        .process_room_redaction(redaction("$redact-root:hs", 500, "$root:hs"))
        .await
        .expect("redact root");
    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(
        captured.len(),
        1,
        "root redaction must not emit a summary snapshot"
    );
    assert!(matches!(
        &captured[0],
        cumments_core::projector_events::ProjectorEvent::MessageDeleted { event_id, .. }
            if event_id == "$root:hs"
    ));

    // A replayed redaction is a duplicate delete without a fresh snapshot.
    processor.start_event_capture().await;
    processor
        .process_room_redaction(redaction("$redact-again:hs", 600, "$b:hs"))
        .await
        .expect("replay member b redaction");
    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 1);
    assert!(matches!(
        &captured[0],
        cumments_core::projector_events::ProjectorEvent::MessageDeleted { event_id, .. }
            if event_id == "$b:hs"
    ));
}

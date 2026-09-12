//! Poll realtime semantics: Poll starts emit `PollCreated`, vote changes
//! emit `PollVoted`, and ends emit `PollEnded`. Redundant or ineffective
//! operations emit nothing, and annotations are never leaked into `MessageAnnotationsChanged`.

use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, Message, MessageStatus, PollContent, PollOption,
    RoomIdentity, TextContent, TextStyle,
};
use cumments_core::poll::{PollSemanticKind, PollStatus};
use cumments_core::ports::{MessageStore, RegistryStore};
use cumments_core::projector_events::ProjectorEvent;
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::{
    ParsedPollEnd, ParsedPollVote, ParsedRoomMessage, ParsedRoomRedaction,
};
use cumments_store::DbStore;
use std::sync::Arc;
use tokio::sync::broadcast;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-poll-realtime-{}-{}.db",
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
    let (tx, _rx) = broadcast::channel(32);
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

fn poll_message(
    event_id: &str,
    question: &str,
    answers: &[(&str, &str)],
    kind: PollSemanticKind,
    ts: i64,
) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: "!room:hs".to_string(),
        event_id: event_id.to_string(),
        event_type: "org.matrix.msc3381.poll.start".to_string(),
        sender: "@creator:hs".to_string(),
        content: Content::Poll(PollContent {
            question: question.to_string(),
            answers: answers
                .iter()
                .map(|(id, text)| PollOption {
                    id: id.to_string(),
                    text: text.to_string(),
                })
                .collect(),
            kind,
            max_selections: 1,
            status: PollStatus::Open,
            end_time: None,
            results: None,
            total_votes: 0,
            responses: Vec::new(),
            my_votes: None,
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
        room_identity: Some(identity()),
        raw_content: serde_json::Value::Null,
    }
}

fn text_message(event_id: &str, text: &str, ts: i64) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: "!room:hs".to_string(),
        event_id: event_id.to_string(),
        event_type: "m.room.message".to_string(),
        sender: "@alice:hs".to_string(),
        content: Content::Text(TextContent {
            body: text.to_string(),
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
        room_identity: Some(identity()),
        raw_content: serde_json::Value::Null,
    }
}

fn poll_vote(
    event_id: &str,
    poll_id: &str,
    sender: &str,
    answers: &[&str],
    ts: i64,
) -> ParsedPollVote {
    ParsedPollVote {
        room_id: "!room:hs".to_string(),
        event_id: event_id.to_string(),
        poll_message_id: poll_id.to_string(),
        sender: sender.to_string(),
        origin_server_ts: ts,
        answer_ids: answers.iter().map(|s| s.to_string()).collect(),
        author_public_key: None,
        author_signature: None,
        author_challenge: None,
        is_virtual_user_sender: false,
        room_identity: Some(identity()),
    }
}

fn poll_end(event_id: &str, poll_id: &str, sender: &str, ts: i64) -> ParsedPollEnd {
    ParsedPollEnd {
        room_id: "!room:hs".to_string(),
        event_id: event_id.to_string(),
        poll_message_id: poll_id.to_string(),
        sender: sender.to_string(),
        origin_server_ts: ts,
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
async fn poll_created_emitted_when_poll_start_projected() {
    let (_store, processor) = setup("poll-create").await;

    processor.start_event_capture().await;
    processor
        .process_room_message(poll_message(
            "$poll1:hs",
            "Favorite fruit?",
            &[("apple", "Apple"), ("banana", "Banana")],
            PollSemanticKind::Disclosed,
            100,
        ))
        .await
        .expect("process poll start");

    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 1, "poll start emits exactly PollCreated");
    match &captured[0] {
        ProjectorEvent::PollCreated {
            site_id,
            page_slug,
            poll_id,
            message,
        } => {
            assert_eq!(site_id, "my-blog");
            assert_eq!(page_slug, "hello");
            assert_eq!(poll_id, "$poll1:hs");
            assert_eq!(message.event_id, "$poll1:hs");
            let Content::Poll(ref poll) = message.content else {
                panic!("expected poll content");
            };
            assert_eq!(poll.question, "Favorite fruit?");
            assert_eq!(poll.answers.len(), 2);
            assert_eq!(poll.status, PollStatus::Open);
            assert_eq!(poll.total_votes, 0);
            assert_eq!(poll.my_votes, None, "public SSE payload has my_votes: None");
        }
        other => panic!("expected PollCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn poll_voted_emitted_when_effective_projection_changes() {
    let (_store, processor) = setup("poll-voted").await;

    // Seed the poll
    processor
        .process_room_message(poll_message(
            "$poll:hs",
            "Favorite fruit?",
            &[("apple", "Apple"), ("banana", "Banana")],
            PollSemanticKind::Disclosed,
            100,
        ))
        .await
        .expect("seed poll");

    // Vote 1: Alice votes "apple". Effective tally changes.
    processor.start_event_capture().await;
    processor
        .process_poll_vote(poll_vote(
            "$v1:hs",
            "$poll:hs",
            "@alice:hs",
            &["apple"],
            200,
        ))
        .await
        .expect("vote 1");
    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 1, "first vote emits PollVoted");
    match &captured[0] {
        ProjectorEvent::PollVoted {
            site_id,
            page_slug,
            poll_id,
            results,
            total_votes,
        } => {
            assert_eq!(site_id, "my-blog");
            assert_eq!(page_slug, "hello");
            assert_eq!(poll_id, "$poll:hs");
            assert_eq!(*total_votes, 1);
            let res = results
                .as_ref()
                .expect("results exposed for disclosed poll");
            assert_eq!(res.get("apple").and_then(|v| v.as_u64()), Some(1));
            assert_eq!(res.get("banana").and_then(|v| v.as_u64()), Some(0));
        }
        other => panic!("expected PollVoted, got {other:?}"),
    }

    // Vote 2 (duplicate/redundant): Alice sends the exact same vote again.
    processor.start_event_capture().await;
    processor
        .process_poll_vote(poll_vote(
            "$v2:hs",
            "$poll:hs",
            "@alice:hs",
            &["apple"],
            250,
        ))
        .await
        .expect("redundant vote");
    let captured2 = processor.stop_event_capture().await.expect("captured");
    assert!(
        captured2.is_empty(),
        "redundant vote that does not change projection emits nothing"
    );

    // Vote 3: Alice changes vote from "apple" to "banana".
    processor.start_event_capture().await;
    processor
        .process_poll_vote(poll_vote(
            "$v3:hs",
            "$poll:hs",
            "@alice:hs",
            &["banana"],
            300,
        ))
        .await
        .expect("vote change");
    let captured3 = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured3.len(), 1, "vote change emits PollVoted");
    match &captured3[0] {
        ProjectorEvent::PollVoted {
            results,
            total_votes,
            ..
        } => {
            assert_eq!(*total_votes, 1);
            let res = results.as_ref().expect("results exposed");
            assert_eq!(res.get("apple").and_then(|v| v.as_u64()), Some(0));
            assert_eq!(res.get("banana").and_then(|v| v.as_u64()), Some(1));
        }
        other => panic!("expected PollVoted, got {other:?}"),
    }

    // Redaction: Redact Alice's latest vote ($v3:hs). Her previous vote ($v2:hs / $v1:hs) is restored ("apple").
    processor.start_event_capture().await;
    processor
        .process_room_redaction(ParsedRoomRedaction {
            room_id: "!room:hs".to_string(),
            event_id: "$redact-v3:hs".to_string(),
            sender: Some("@alice:hs".to_string()),
            origin_server_ts: 400,
            redacts: Some("$v3:hs".to_string()),
            proof: None,
            submission_id: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("redact vote 3");
    let captured4 = processor.stop_event_capture().await.expect("captured");
    assert_eq!(
        captured4.len(),
        1,
        "redacting vote rolls back tally and emits PollVoted"
    );
    match &captured4[0] {
        ProjectorEvent::PollVoted {
            results,
            total_votes,
            ..
        } => {
            assert_eq!(*total_votes, 1);
            let res = results.as_ref().expect("results exposed");
            assert_eq!(res.get("apple").and_then(|v| v.as_u64()), Some(1));
            assert_eq!(res.get("banana").and_then(|v| v.as_u64()), Some(0));
        }
        other => panic!("expected PollVoted, got {other:?}"),
    }
}

#[tokio::test]
async fn poll_voted_masks_results_when_undisclosed_and_open() {
    let (_store, processor) = setup("poll-undisclosed-voted").await;

    // Undisclosed poll
    processor
        .process_room_message(poll_message(
            "$undisc:hs",
            "Secret vote",
            &[("x", "Option X"), ("y", "Option Y")],
            PollSemanticKind::Undisclosed,
            100,
        ))
        .await
        .expect("seed undisclosed poll");

    processor.start_event_capture().await;
    processor
        .process_poll_vote(poll_vote("$v1:hs", "$undisc:hs", "@bob:hs", &["x"], 200))
        .await
        .expect("bob votes");

    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 1);
    match &captured[0] {
        ProjectorEvent::PollVoted {
            results,
            total_votes,
            ..
        } => {
            assert_eq!(*total_votes, 1);
            assert!(
                results.is_none(),
                "undisclosed open poll must mask results in PollVoted (results: null)"
            );
        }
        other => panic!("expected PollVoted, got {other:?}"),
    }
}

#[tokio::test]
async fn poll_ended_emitted_on_transition_and_redundant_emits_nothing() {
    let (_store, processor) = setup("poll-ended").await;

    // Seed undisclosed poll
    processor
        .process_room_message(poll_message(
            "$poll:hs",
            "Vote question",
            &[("opt1", "1"), ("opt2", "2")],
            PollSemanticKind::Undisclosed,
            100,
        ))
        .await
        .expect("seed poll");

    // Alice votes for opt1
    processor
        .process_poll_vote(poll_vote("$v1:hs", "$poll:hs", "@alice:hs", &["opt1"], 200))
        .await
        .expect("alice votes");

    // End 1: First valid end closes the poll
    processor.start_event_capture().await;
    processor
        .process_poll_end(poll_end("$end1:hs", "$poll:hs", "@creator:hs", 300))
        .await
        .expect("first valid end");

    let captured = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured.len(), 1, "first end emits PollEnded");
    match &captured[0] {
        ProjectorEvent::PollEnded {
            site_id,
            page_slug,
            poll_id,
            results,
            total_votes,
            end_time,
        } => {
            assert_eq!(site_id, "my-blog");
            assert_eq!(page_slug, "hello");
            assert_eq!(poll_id, "$poll:hs");
            assert_eq!(*total_votes, 1);
            assert_eq!(*end_time, 300);
            // Even though kind was undisclosed, ended poll exposes unmasked results!
            assert_eq!(results.get("opt1").and_then(|v| v.as_u64()), Some(1));
            assert_eq!(results.get("opt2").and_then(|v| v.as_u64()), Some(0));
        }
        other => panic!("expected PollEnded, got {other:?}"),
    }

    // End 2: Redundant end after poll is already ended emits NOTHING
    processor.start_event_capture().await;
    processor
        .process_poll_end(poll_end("$end2:hs", "$poll:hs", "@creator:hs", 400))
        .await
        .expect("second end");

    let captured2 = processor.stop_event_capture().await.expect("captured");
    assert!(captured2.is_empty(), "redundant end emits nothing");
}

#[tokio::test]
async fn poll_created_deduplicates_on_replay_and_repeated_processing() {
    let (_store, processor) = setup("poll-create-dedup").await;

    // First processing of poll start
    processor.start_event_capture().await;
    processor
        .process_room_message(poll_message(
            "$poll-dup:hs",
            "Favorite color?",
            &[("red", "Red"), ("blue", "Blue")],
            PollSemanticKind::Disclosed,
            100,
        ))
        .await
        .expect("process poll start");

    let captured1 = processor.stop_event_capture().await.expect("captured");
    assert_eq!(
        captured1.len(),
        1,
        "first processing emits exactly one PollCreated"
    );
    assert!(
        matches!(&captured1[0], ProjectorEvent::PollCreated { poll_id, .. } if poll_id == "$poll-dup:hs")
    );

    // Second processing of the exact same poll start (duplicate delivery / replay)
    processor.start_event_capture().await;
    processor
        .process_room_message(poll_message(
            "$poll-dup:hs",
            "Favorite color?",
            &[("red", "Red"), ("blue", "Blue")],
            PollSemanticKind::Disclosed,
            100,
        ))
        .await
        .expect("reprocess poll start");

    let captured2 = processor.stop_event_capture().await.expect("captured");
    assert!(
        captured2.is_empty(),
        "reprocessing existing poll start must emit no additional PollCreated"
    );
}

#[tokio::test]
async fn non_poll_comment_repeated_processing_remains_unchanged() {
    let (_store, processor) = setup("non-poll-repeat").await;

    // First processing of text comment
    processor.start_event_capture().await;
    processor
        .process_room_message(text_message("$text1:hs", "Hello world", 100))
        .await
        .expect("process text message");

    let captured1 = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured1.len(), 1);
    assert!(
        matches!(&captured1[0], ProjectorEvent::MessageCreated { message, .. } if message.event_id == "$text1:hs")
    );

    // Repeated processing of text comment: behavior remains unchanged (emits MessageCreated)
    processor.start_event_capture().await;
    processor
        .process_room_message(text_message("$text1:hs", "Hello world", 100))
        .await
        .expect("reprocess text message");

    let captured2 = processor.stop_event_capture().await.expect("captured");
    assert_eq!(captured2.len(), 1);
    assert!(
        matches!(&captured2[0], ProjectorEvent::MessageCreated { message, .. } if message.event_id == "$text1:hs")
    );
}

#[tokio::test]
async fn already_projected_poll_does_not_emit_poll_created_on_rebuild() {
    let (store, processor) = setup("poll-already-projected").await;

    // Seed poll directly in store (as if backfilled / projected previously)
    let msg = Message {
        event_id: "$poll-preseeded:hs".to_string(),
        site_id: "my-blog".to_string(),
        page_slug: "hello".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Matrix,
            display_name: Some("Creator".to_string()),
            avatar_url: None,
            public_key: None,
            mxid: Some("@creator:hs".to_string()),
        },
        content: Content::Poll(PollContent {
            question: "Pre-existing question?".to_string(),
            answers: vec![
                PollOption {
                    id: "a".to_string(),
                    text: "A".to_string(),
                },
                PollOption {
                    id: "b".to_string(),
                    text: "B".to_string(),
                },
            ],
            kind: PollSemanticKind::Disclosed,
            max_selections: 1,
            status: PollStatus::Open,
            end_time: None,
            results: None,
            total_votes: 0,
            responses: Vec::new(),
            my_votes: None,
        }),
        matrix_event_type: "org.matrix.msc3381.poll.start".to_string(),
        timestamp: chrono::Utc::now(),
        edited_at: None,
        reply_to: None,
        thread_root: None,
        submission_id: None,
        status: MessageStatus::Active,
        redacted_at: None,
        redacted_by: None,
        reactions: Vec::new(),
        thread_summary: None,
        room_id: "!room:hs".to_string(),
        sender_mxid: "@creator:hs".to_string(),
        raw_content: serde_json::Value::Null,
    };
    store.save_message(&msg).await.expect("preseed message");

    // Rebuilding / reprocessing this poll start via event processor
    processor.start_event_capture().await;
    processor
        .process_room_message(poll_message(
            "$poll-preseeded:hs",
            "Pre-existing question?",
            &[("a", "A"), ("b", "B")],
            PollSemanticKind::Disclosed,
            100,
        ))
        .await
        .expect("process already projected poll");

    let captured = processor.stop_event_capture().await.expect("captured");
    assert!(
        captured.is_empty(),
        "reprocessing an already-projected poll start must not emit PollCreated"
    );
}

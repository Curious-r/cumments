use cumments_core::models::{
    Content, PollContent, PollOption, PostSlug, RoomIdentity, SiteId, TextContent, TextStyle,
};
use cumments_core::ports::{MessageStore, RegistryStore};
use cumments_projector::event_processor::EventProcessor;
use cumments_projector::parsed::{
    ParsedPollVote, ParsedReaction, ParsedRoomMessage, ParsedRoomRedaction,
};
use cumments_store::DbStore;
use std::sync::Arc;
use tokio::sync::broadcast;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-tombstone-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn identity() -> RoomIdentity {
    RoomIdentity {
        site_id: "my-blog".to_string(),
        post_slug: "hello".to_string(),
    }
}

async fn processor(store: Arc<DbStore>) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(
        store.clone() as Arc<dyn cumments_core::ports::SiteStore>,
        store.clone() as Arc<dyn cumments_core::ports::RegistryStore>,
        store.clone() as Arc<dyn cumments_core::ports::MessageStore>,
        store.clone() as Arc<dyn cumments_core::ports::RoomStore>,
        store.clone() as Arc<dyn cumments_core::ports::IntentStore>,
        tx,
        None,
    )
}

fn message(event_id: &str) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: "!room:hs".to_string(),
        event_id: event_id.to_string(),
        sender: "@alice:hs".to_string(),
        content: Content::Text(TextContent {
            body: "resurrected".to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        display_name: None,
        author_public_key: None,
        author_signature: None,
        author_challenge: None,
        is_virtual_user_sender: false,
        intent_id: None,
        reply_to: None,
        thread_root: None,
        origin_server_ts: 100,
        relates_to: None,
        room_identity: Some(identity()),
        raw_content: serde_json::Value::Null,
    }
}

#[tokio::test]
async fn reaction_redaction_removes_it_and_prevents_resurrection() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("reaction-redact"))
            .await
            .expect("connect db"),
    );
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");

    let processor = processor(store.clone()).await;
    processor
        .process_room_message(message("$target:hs"))
        .await
        .expect("process message");
    processor
        .process_reaction(ParsedReaction {
            room_id: "!room:hs".to_string(),
            event_id: "$reaction:hs".to_string(),
            sender: "@bob:hs".to_string(),
            message_event_id: "$target:hs".to_string(),
            key: "👍".to_string(),
            origin_server_ts: 200,
            is_virtual_user_sender: false,
            author_public_key: None,
            author_signature: None,
            author_challenge: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("process reaction");

    let reaction_count = || async {
        store
            .get_message("$target:hs")
            .await
            .expect("query message")
            .expect("message exists")
            .reactions
            .iter()
            .map(|r| r.count)
            .sum::<i64>()
    };
    assert_eq!(reaction_count().await, 1);

    processor
        .process_room_redaction(ParsedRoomRedaction {
            room_id: "!room:hs".to_string(),
            event_id: "$redaction:hs".to_string(),
            sender: Some("@admin:hs".to_string()),
            origin_server_ts: 300,
            redacts: Some("$reaction:hs".to_string()),
            proof: None,
            intent_id: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("process reaction redaction");
    assert_eq!(
        reaction_count().await,
        0,
        "redacted reaction must leave the aggregate"
    );

    // Re-delivering the original reaction must not resurrect it.
    processor
        .process_reaction(ParsedReaction {
            room_id: "!room:hs".to_string(),
            event_id: "$reaction:hs".to_string(),
            sender: "@bob:hs".to_string(),
            message_event_id: "$target:hs".to_string(),
            key: "👍".to_string(),
            origin_server_ts: 200,
            is_virtual_user_sender: false,
            author_public_key: None,
            author_signature: None,
            author_challenge: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("re-deliver reaction");
    assert_eq!(
        reaction_count().await,
        0,
        "tombstoned reaction must not resurrect"
    );
}

#[tokio::test]
async fn poll_vote_redaction_removes_it_and_prevents_resurrection() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("vote-redact"))
            .await
            .expect("connect db"),
    );
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");

    let processor = processor(store.clone()).await;
    let mut poll = message("$poll:hs");
    poll.content = Content::Poll(PollContent {
        question: "best? ".to_string(),
        options: vec![PollOption {
            id: "a".to_string(),
            text: "A".to_string(),
        }],
        responses: Vec::new(),
    });
    processor
        .process_room_message(poll)
        .await
        .expect("process poll");
    processor
        .process_poll_vote(ParsedPollVote {
            room_id: "!room:hs".to_string(),
            event_id: "$vote:hs".to_string(),
            sender: "@bob:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            answer_ids: vec!["a".to_string()],
            origin_server_ts: 200,
            is_virtual_user_sender: false,
            author_public_key: None,
            author_signature: None,
            author_challenge: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("process vote");

    let vote_count = || async {
        match store
            .get_message("$poll:hs")
            .await
            .expect("query poll")
            .expect("poll exists")
            .content
        {
            Content::Poll(poll) => poll.responses.iter().map(|r| r.count).sum::<i64>(),
            other => panic!("expected poll content, got {other:?}"),
        }
    };
    assert_eq!(vote_count().await, 1);

    processor
        .process_room_redaction(ParsedRoomRedaction {
            room_id: "!room:hs".to_string(),
            event_id: "$redaction:hs".to_string(),
            sender: Some("@admin:hs".to_string()),
            origin_server_ts: 300,
            redacts: Some("$vote:hs".to_string()),
            proof: None,
            intent_id: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("process vote redaction");
    assert_eq!(
        vote_count().await,
        0,
        "redacted vote must leave the aggregate"
    );

    // Re-delivering the original vote must not resurrect it.
    processor
        .process_poll_vote(ParsedPollVote {
            room_id: "!room:hs".to_string(),
            event_id: "$vote:hs".to_string(),
            sender: "@bob:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            answer_ids: vec!["a".to_string()],
            origin_server_ts: 200,
            is_virtual_user_sender: false,
            author_public_key: None,
            author_signature: None,
            author_challenge: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("re-deliver vote");
    assert_eq!(vote_count().await, 0, "tombstoned vote must not resurrect");
}

#[tokio::test]
async fn redaction_seen_before_target_prevents_resurrection() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("resurrect"))
            .await
            .expect("connect db"),
    );
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");

    let processor = processor(store.clone()).await;

    // Backfill run 1: only the redaction is in the fetched window.
    processor
        .process_room_redaction(ParsedRoomRedaction {
            room_id: "!room:hs".to_string(),
            event_id: "$redaction:hs".to_string(),
            sender: Some("@admin:hs".to_string()),
            origin_server_ts: 100,
            redacts: Some("$target:hs".to_string()),
            proof: None,
            intent_id: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("process redaction");

    // Backfill run 2: the original is fetched later and must be suppressed.
    processor
        .process_room_message(message("$target:hs"))
        .await
        .expect("process message");

    let stored = store
        .get_message("$target:hs")
        .await
        .expect("query message");
    assert!(stored.is_none(), "tombstoned message must not resurrect");
}

#[tokio::test]
async fn message_without_tombstone_is_projected() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("normal"))
            .await
            .expect("connect db"),
    );
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");

    let processor = processor(store.clone()).await;
    processor
        .process_room_message(message("$target:hs"))
        .await
        .expect("process message");

    let stored = store
        .get_message("$target:hs")
        .await
        .expect("query message");
    assert!(stored.is_some(), "normal message must be projected");
}

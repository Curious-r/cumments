use cumments_core::commands::UpdateCommentCommand;
use cumments_core::identity::{derive_visitor_id_from_public_key, signature_message};
use cumments_core::models::{
    Content, LocationContent, PageSlug, PollContent, PollOption, RoomIdentity, SiteId, TextContent,
    TextStyle,
};
use cumments_core::ports::{MessageStore, RegistryStore, SubmissionStore};
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::{
    ParsedPollVote, ParsedReaction, ParsedRelation, ParsedRoomMessage, ParsedRoomRedaction,
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
        page_slug: "hello".to_string(),
    }
}

async fn processor(store: Arc<DbStore>) -> EventProcessor {
    processor_named(store, None).await
}

async fn processor_named(store: Arc<DbStore>, server_name: Option<&str>) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(EventProcessorDeps {
        site_store: store.clone() as Arc<dyn cumments_core::ports::SiteStore>,
        registry_store: store.clone() as Arc<dyn cumments_core::ports::RegistryStore>,
        message_store: store.clone() as Arc<dyn cumments_core::ports::MessageStore>,
        room_store: store.clone() as Arc<dyn cumments_core::ports::RoomStore>,
        governance_store: store.clone() as Arc<dyn cumments_core::ports::GovernanceStore>,
        sticker_pack_store: store.clone() as Arc<dyn cumments_core::ports::StickerPackStore>,
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
        server_name: server_name.map(|s| s.to_string()),
    })
}

fn message(event_id: &str) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: "!room:hs".to_string(),
        event_id: event_id.to_string(),
        event_type: "m.room.message".to_string(),
        sender: "@alice:hs".to_string(),
        content: Content::Text(TextContent {
            body: "resurrected".to_string(),
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
        origin_server_ts: 100,
        relates_to: None,
        room_identity: Some(identity()),
        raw_content: serde_json::Value::Null,
    }
}

#[tokio::test]
async fn visitor_location_verifies_with_locate_signature() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    let store = Arc::new(
        DbStore::connect(&test_db_url("visitor-location"))
            .await
            .expect("connect db"),
    );
    let site = SiteId::from("my-blog");
    let slug = PageSlug::from("hello");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");
    let processor = processor_named(store.clone(), Some("example.com")).await;

    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let visitor_id = derive_visitor_id_from_public_key(&public_key).expect("visitor id");
    let sender = format!("@_cumments_my-blog_{}:example.com", visitor_id);
    let challenge = "challenge";
    let geo_uri = "geo:31.2,121.5";
    let signed_message = signature_message(&[
        Some("LOCATE"),
        Some("my-blog"),
        Some("hello"),
        Some(geo_uri),
        None,
        None,
        Some(challenge),
    ]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(signed_message.as_bytes()).to_bytes());

    let mut location = message("$loc:hs");
    location.content = Content::Location(LocationContent {
        geo_uri: geo_uri.to_string(),
        description: Some("here".to_string()),
        thumbnail_url: None,
    });
    location.sender = sender.clone();
    location.author_public_key = Some(public_key);
    location.author_signature = Some(signature);
    location.author_challenge = Some(challenge.to_string());
    location.is_virtual_user_sender = true;
    processor
        .process_room_message(location)
        .await
        .expect("process location");

    assert!(
        store
            .get_message("$loc:hs")
            .await
            .expect("query location")
            .is_some(),
        "visitor location with a valid LOCATE signature must project"
    );

    // A POST-format signature (what text/media use) must be rejected for
    // locations; this locks the LOCATE-specific verification path.
    let mut wrong = message("$loc-bad:hs");
    wrong.content = Content::Location(LocationContent {
        geo_uri: geo_uri.to_string(),
        description: None,
        thumbnail_url: None,
    });
    wrong.sender = sender.clone();
    wrong.author_public_key = Some(URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()));
    let wrong_message = signature_message(&[
        Some("POST"),
        Some("my-blog"),
        Some("hello"),
        Some(geo_uri),
        None,
        None,
        Some(challenge),
    ]);
    wrong.author_signature =
        Some(URL_SAFE_NO_PAD.encode(signing_key.sign(wrong_message.as_bytes()).to_bytes()));
    wrong.author_challenge = Some(challenge.to_string());
    wrong.is_virtual_user_sender = true;
    processor
        .process_room_message(wrong)
        .await
        .expect("process wrong location");
    assert!(
        store
            .get_message("$loc-bad:hs")
            .await
            .expect("query wrong location")
            .is_none(),
        "visitor location signed with the POST format must be rejected"
    );
}

#[tokio::test]
async fn edit_redaction_rolls_the_parent_back_to_original_content() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("edit-redaction"))
            .await
            .expect("connect db"),
    );
    store
        .register_room(
            "!room:hs",
            &SiteId::from("my-blog"),
            &PageSlug::from("hello"),
        )
        .await
        .expect("register room");
    let processor = processor(store.clone()).await;

    processor
        .process_room_message(message("$original:hs"))
        .await
        .expect("process original");

    let mut replacement = message("$replace:hs");
    replacement.origin_server_ts = 200;
    replacement.content = Content::Text(TextContent {
        body: "edited".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    replacement.relates_to = Some(ParsedRelation {
        target_event_id: "$original:hs".to_string(),
        new_content: replacement.content.clone(),
    });
    processor
        .process_room_message(replacement)
        .await
        .expect("process replacement");

    let edited = store
        .get_message("$original:hs")
        .await
        .expect("query edited parent")
        .expect("parent exists");
    assert!(
        matches!(edited.content, Content::Text(ref text) if text.body == "edited"),
        "replacement must become the current view"
    );

    processor
        .process_room_redaction(ParsedRoomRedaction {
            room_id: "!room:hs".to_string(),
            event_id: "$redaction:hs".to_string(),
            sender: Some("@alice:hs".to_string()),
            origin_server_ts: 300,
            redacts: Some("$replace:hs".to_string()),
            proof: None,
            submission_id: None,
            room_identity: Some(identity()),
        })
        .await
        .expect("redact replacement");

    let rolled_back = store
        .get_message("$original:hs")
        .await
        .expect("query rolled-back parent")
        .expect("parent exists");
    assert!(
        matches!(rolled_back.content, Content::Text(ref text) if text.body == "resurrected"),
        "redacting the only replacement must restore original content"
    );
    assert!(rolled_back.edited_at.is_none());
    let revision = store
        .get_message_revision("$replace:hs")
        .await
        .expect("query revision")
        .expect("revision remains as a redacted fact");
    assert!(revision.redacted_at.is_some());
}

#[tokio::test]
async fn replayed_edit_closes_a_submission_after_a_projection_crash() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("edit-replay-closure"))
            .await
            .expect("connect db"),
    );
    store
        .register_room(
            "!room:hs",
            &SiteId::from("my-blog"),
            &PageSlug::from("hello"),
        )
        .await
        .expect("register room");
    let processor = processor(store.clone()).await;
    processor
        .process_room_message(message("$original:hs"))
        .await
        .expect("process original");

    // Simulate a crash after the fact was committed but before the local
    // submission was completed.
    let mut parent = store
        .get_message("$original:hs")
        .await
        .expect("query parent")
        .expect("parent exists");
    let edited_at = chrono::DateTime::from_timestamp_millis(200).expect("valid timestamp");
    parent.content = Content::Text(TextContent {
        body: "edited".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    parent.edited_at = Some(edited_at);
    store
        .apply_edit(
            &parent,
            &cumments_core::models::MessageRevision {
                event_id: "$replace:hs".to_string(),
                message_event_id: "$original:hs".to_string(),
                content: parent.content.clone(),
                edited_at,
                editor_mxid: parent.sender_mxid.clone(),
                redacted_at: None,
            },
        )
        .await
        .expect("seed applied revision");

    let submission_id = store
        .save_update_submission(&UpdateCommentCommand {
            site_id: SiteId::from("my-blog"),
            page_slug: PageSlug::from("hello"),
            event_id: "$original:hs".to_string(),
            content: "edited".to_string(),
            author_public_key: String::new(),
            author_signature: String::new(),
            author_challenge: String::new(),
        })
        .await
        .expect("queue update");

    let mut replacement = message("$replace:hs");
    replacement.submission_id = Some(submission_id);
    replacement.origin_server_ts = 200;
    replacement.content = Content::Text(TextContent {
        body: "edited".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    replacement.relates_to = Some(ParsedRelation {
        target_event_id: "$original:hs".to_string(),
        new_content: replacement.content.clone(),
    });
    processor
        .process_room_message(replacement)
        .await
        .expect("replay replacement");

    assert!(
        store
            .get_pending_update_submissions(10)
            .await
            .expect("query pending submissions")
            .is_empty(),
        "already-known replacement must close its correlated submission"
    );
}

#[tokio::test]
async fn reaction_redaction_removes_it_and_prevents_resurrection() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("reaction-redact"))
            .await
            .expect("connect db"),
    );
    let site = SiteId::from("my-blog");
    let slug = PageSlug::from("hello");
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
            sender: Some(":hs".to_string()),
            origin_server_ts: 300,
            redacts: Some("$reaction:hs".to_string()),
            proof: None,
            submission_id: None,
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
    let slug = PageSlug::from("hello");
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
            sender: Some(":hs".to_string()),
            origin_server_ts: 300,
            redacts: Some("$vote:hs".to_string()),
            proof: None,
            submission_id: None,
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
    let slug = PageSlug::from("hello");
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
            sender: Some(":hs".to_string()),
            origin_server_ts: 100,
            redacts: Some("$target:hs".to_string()),
            proof: None,
            submission_id: None,
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
    let slug = PageSlug::from("hello");
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

use chrono::Utc;
use cumments_core::media_upload::{MediaUploadIdempotencyInput, MediaUploadIdempotencyOutcome};
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, MediaContent, MediaKind, Message, MessageRevision,
    MessageStatus, PollContent, PollOption, PollVote, PostSlug, Reaction, SiteId, TextContent,
    TextStyle, UnknownContent,
};
use cumments_core::ports::{MessageStore, VirtualUserStore};
use cumments_store::DbStore;

/// Unique SQLite file per test to avoid shared in-memory state.
fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-message-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn guest_message(event_id: &str, body: &str) -> Message {
    Message {
        event_id: event_id.to_string(),
        site_id: "my-blog".to_string(),
        post_slug: "hello".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Guest,
            display_name: Some("Alice".to_string()),
            avatar_url: None,
            public_key: Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string()),
            mxid: None,
        },
        content: Content::Text(TextContent {
            body: body.to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        timestamp: Utc::now(),
        edited_at: None,
        reply_to: Some("$parent:hs".to_string()),
        thread_root: None,
        submission_id: Some(42),
        status: MessageStatus::Active,
        redacted_at: None,
        redacted_by: None,
        reactions: Vec::new(),
        room_id: "!room:hs".to_string(),
        sender_mxid: "@_cumments_my-blog_a1b2c3d4e5f60718:hs".to_string(),
        raw_content: serde_json::json!({ "msgtype": "m.text", "body": body }),
    }
}

#[tokio::test]
async fn save_message_records_typed_content_and_internal_fields() {
    let store = DbStore::connect(&test_db_url("message-sender"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");

    let message = guest_message("$event:hs", "hello");
    store.save_message(&message).await.expect("save message");

    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(stored.room_id, "!room:hs");
    assert_eq!(stored.sender_mxid, "@_cumments_my-blog_a1b2c3d4e5f60718:hs");
    assert_eq!(stored.author.kind, AuthorKind::Guest);
    assert_eq!(
        stored.author.public_key.as_deref(),
        Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc")
    );
    assert_eq!(stored.reply_to.as_deref(), Some("$parent:hs"));
    assert_eq!(stored.submission_id, Some(42));
    assert_eq!(
        stored.content,
        Content::Text(TextContent {
            body: "hello".to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        })
    );
    assert_eq!(stored.status, MessageStatus::Active);

    let page = store
        .get_messages(&site, &slug, 10, 0)
        .await
        .expect("query messages");
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].event_id, "$event:hs");
}

#[tokio::test]
async fn apply_edit_updates_content_and_records_revision() {
    let store = DbStore::connect(&test_db_url("message-edit"))
        .await
        .expect("connect db");
    let message = guest_message("$event:hs", "original");
    store.save_message(&message).await.expect("save message");

    let mut updated = message.clone();
    updated.content = Content::Text(TextContent {
        body: "edited".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    updated.edited_at = Some(Utc::now());
    let revision = MessageRevision {
        event_id: "$edit:hs".to_string(),
        content: updated.content.clone(),
        edited_at: updated.edited_at.unwrap(),
        editor_mxid: "@_cumments_my-blog_a1b2c3d4e5f60718:hs".to_string(),
    };

    assert!(
        store
            .apply_edit(&updated, &revision)
            .await
            .expect("apply edit")
    );
    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert!(matches!(
        stored.content,
        Content::Text(ref t) if t.body == "edited"
    ));
    assert!(stored.edited_at.is_some());
    assert_eq!(stored.reply_to.as_deref(), Some("$parent:hs"));

    // A stale edit (older timestamp) must be rejected.
    let stale_ts = revision.edited_at - chrono::Duration::seconds(1);
    let stale = MessageRevision {
        event_id: "$stale:hs".to_string(),
        content: updated.content.clone(),
        edited_at: stale_ts,
        editor_mxid: "someone".to_string(),
    };
    assert!(
        !store
            .apply_edit(&updated, &stale)
            .await
            .expect("stale edit rejected")
    );
}

#[tokio::test]
async fn redact_message_marks_status_and_keeps_row() {
    let store = DbStore::connect(&test_db_url("message-redact"))
        .await
        .expect("connect db");
    let message = guest_message("$event:hs", "hello");
    store.save_message(&message).await.expect("save message");

    let now = Utc::now();
    assert!(
        store
            .redact_message("$event:hs", "!room:hs", now, "@admin:hs")
            .await
            .expect("redact message")
    );
    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(stored.status, MessageStatus::Redacted);
    assert_eq!(stored.redacted_by.as_deref(), Some("@admin:hs"));

    assert!(
        !store
            .redact_message("$missing:hs", "!room:hs", now, "@admin:hs")
            .await
            .expect("missing target")
    );
}

#[tokio::test]
async fn reactions_aggregate_by_key_and_ignore_redacted() {
    let store = DbStore::connect(&test_db_url("message-reactions"))
        .await
        .expect("connect db");
    let message = guest_message("$event:hs", "hello");
    store.save_message(&message).await.expect("save message");

    for (event_id, sender) in [
        ("$r1:hs", "@alice:hs"),
        ("$r2:hs", "@bob:hs"),
        ("$r3:hs", "@carol:hs"),
    ] {
        store
            .save_reaction(&Reaction {
                event_id: event_id.to_string(),
                message_event_id: "$event:hs".to_string(),
                sender_mxid: sender.to_string(),
                key: "👍".to_string(),
                origin_server_ts: 1,
                redacted_at: None,
            })
            .await
            .expect("save reaction");
    }
    store
        .save_reaction(&Reaction {
            event_id: "$r4:hs".to_string(),
            message_event_id: "$event:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            key: "❤️".to_string(),
            origin_server_ts: 2,
            redacted_at: None,
        })
        .await
        .expect("save reaction");

    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(stored.reactions.len(), 2);
    assert_eq!(stored.reactions[0].key, "❤️");
    assert_eq!(stored.reactions[0].count, 1);
    assert_eq!(stored.reactions[1].key, "👍");
    assert_eq!(stored.reactions[1].count, 3);

    // Redacting one reaction drops its sender from the count.
    store
        .redact_reaction("$r1:hs", Utc::now())
        .await
        .expect("redact reaction");
    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    let thumbs = stored
        .reactions
        .iter()
        .find(|r| r.key == "👍")
        .expect("thumbs up");
    assert_eq!(thumbs.count, 2);
}

#[tokio::test]
async fn poll_votes_aggregate_and_latest_vote_wins() {
    let store = DbStore::connect(&test_db_url("message-poll"))
        .await
        .expect("connect db");
    let mut message = guest_message("$poll:hs", "poll placeholder");
    message.content = Content::Poll(PollContent {
        question: "best? ".to_string(),
        options: vec![
            PollOption {
                id: "a".to_string(),
                text: "A".to_string(),
            },
            PollOption {
                id: "b".to_string(),
                text: "B".to_string(),
            },
        ],
        responses: Vec::new(),
    });
    store
        .save_message(&message)
        .await
        .expect("save poll message");

    store
        .save_poll_vote(&PollVote {
            event_id: "$vote-alice-1:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            option_index: 0,
            origin_server_ts: 1,
        })
        .await
        .expect("alice votes");
    store
        .save_poll_vote(&PollVote {
            event_id: "$vote-bob:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@bob:hs".to_string(),
            option_index: 1,
            origin_server_ts: 2,
        })
        .await
        .expect("bob votes");
    // Alice changes her vote; the latest vote wins.
    store
        .save_poll_vote(&PollVote {
            event_id: "$vote-alice-2:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            option_index: 1,
            origin_server_ts: 3,
        })
        .await
        .expect("alice changes vote");

    let stored = store
        .get_message("$poll:hs")
        .await
        .expect("get message")
        .expect("message exists");
    match stored.content {
        Content::Poll(poll) => {
            assert_eq!(poll.responses.len(), 1);
            assert_eq!(poll.responses[0].option_index, 1);
            assert_eq!(poll.responses[0].count, 2);
        }
        other => panic!("expected poll content, got {other:?}"),
    }
}

#[tokio::test]
async fn redacted_poll_votes_leave_the_aggregate_and_do_not_resurrect() {
    let store = DbStore::connect(&test_db_url("message-poll-redact"))
        .await
        .expect("connect db");
    let mut message = guest_message("$poll:hs", "poll placeholder");
    message.content = Content::Poll(PollContent {
        question: "best? ".to_string(),
        options: vec![PollOption {
            id: "a".to_string(),
            text: "A".to_string(),
        }],
        responses: Vec::new(),
    });
    store
        .save_message(&message)
        .await
        .expect("save poll message");

    let bob_vote = PollVote {
        event_id: "$vote-bob:hs".to_string(),
        poll_message_id: "$poll:hs".to_string(),
        sender_mxid: "@bob:hs".to_string(),
        option_index: 0,
        origin_server_ts: 2,
    };
    store.save_poll_vote(&bob_vote).await.expect("bob votes");
    store
        .save_poll_vote(&PollVote {
            event_id: "$vote-alice:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            option_index: 0,
            origin_server_ts: 1,
        })
        .await
        .expect("alice votes");

    assert!(
        store
            .redact_poll_vote("$vote-bob:hs", Utc::now(), "@admin:hs")
            .await
            .expect("redact bob vote")
    );
    let stored = store
        .get_message("$poll:hs")
        .await
        .expect("get message")
        .expect("message exists");
    match stored.content {
        Content::Poll(poll) => {
            assert_eq!(poll.responses.len(), 1);
            assert_eq!(poll.responses[0].count, 1);
        }
        other => panic!("expected poll content, got {other:?}"),
    }

    // Re-delivering the original vote event (push retry / backfill) must not
    // resurrect the redacted vote.
    store
        .save_poll_vote(&bob_vote)
        .await
        .expect("bob vote redelivered");
    let stored = store
        .get_message("$poll:hs")
        .await
        .expect("get message")
        .expect("message exists");
    match stored.content {
        Content::Poll(poll) => {
            assert_eq!(poll.responses.len(), 1);
            assert_eq!(poll.responses[0].count, 1);
        }
        other => panic!("expected poll content, got {other:?}"),
    }
}

#[tokio::test]
async fn stale_poll_vote_redelivery_does_not_overwrite_a_newer_vote() {
    let store = DbStore::connect(&test_db_url("message-poll-stale"))
        .await
        .expect("connect db");
    let mut message = guest_message("$poll:hs", "poll placeholder");
    message.content = Content::Poll(PollContent {
        question: "best? ".to_string(),
        options: vec![
            PollOption {
                id: "a".to_string(),
                text: "A".to_string(),
            },
            PollOption {
                id: "b".to_string(),
                text: "B".to_string(),
            },
        ],
        responses: Vec::new(),
    });
    store
        .save_message(&message)
        .await
        .expect("save poll message");

    store
        .save_poll_vote(&PollVote {
            event_id: "$vote-1:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            option_index: 1,
            origin_server_ts: 3,
        })
        .await
        .expect("newer vote");

    // A stale re-delivery of an older vote must not clobber the newer one.
    store
        .save_poll_vote(&PollVote {
            event_id: "$vote-0:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            option_index: 0,
            origin_server_ts: 1,
        })
        .await
        .expect("stale vote");

    let stored = store
        .get_message("$poll:hs")
        .await
        .expect("get message")
        .expect("message exists");
    match stored.content {
        Content::Poll(poll) => {
            assert_eq!(poll.responses.len(), 1);
            assert_eq!(poll.responses[0].option_index, 1);
            assert_eq!(poll.responses[0].count, 1);
        }
        other => panic!("expected poll content, got {other:?}"),
    }
}

#[tokio::test]
async fn unknown_content_survives_roundtrip() {
    let store = DbStore::connect(&test_db_url("message-unknown"))
        .await
        .expect("connect db");
    let mut message = guest_message("$event:hs", "unused");
    message.content = Content::Unknown(UnknownContent {
        fallback: Some("custom".to_string()),
        raw: serde_json::json!({ "custom": true }),
    });
    store.save_message(&message).await.expect("save message");

    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert!(matches!(stored.content, Content::Unknown(_)));
}

#[tokio::test]
async fn media_uploads_track_ownership_and_usage() {
    let store = DbStore::connect(&test_db_url("media-uploads"))
        .await
        .expect("connect db");

    store
        .record_media_upload("mxc://hs/cat", "alice-key", "my-blog", "hello")
        .await
        .expect("record upload");

    assert!(
        store
            .media_upload_owned_by("mxc://hs/cat", "alice-key", "my-blog", "hello")
            .await
            .expect("ownership check")
    );
    assert!(
        !store
            .media_upload_owned_by("mxc://hs/cat", "bob-key", "my-blog", "hello")
            .await
            .expect("other author rejected")
    );
    assert!(
        !store
            .media_upload_owned_by("mxc://hs/cat", "alice-key", "my-blog", "other")
            .await
            .expect("other post rejected")
    );
    assert!(
        !store
            .media_upload_owned_by("mxc://hs/other", "alice-key", "my-blog", "hello")
            .await
            .expect("unknown url rejected")
    );

    // Re-recording the same URL keeps a single row and re-arms ownership.
    store
        .record_media_upload("mxc://hs/cat", "alice-key", "my-blog", "hello")
        .await
        .expect("re-record upload");

    let unused = store
        .list_unused_media_before(Utc::now() + chrono::Duration::days(1))
        .await
        .expect("list unused");
    assert_eq!(unused, vec!["mxc://hs/cat".to_string()]);

    store
        .mark_media_used("mxc://hs/cat")
        .await
        .expect("mark used");
    let unused = store
        .list_unused_media_before(Utc::now() + chrono::Duration::days(1))
        .await
        .expect("list unused after use");
    assert!(unused.is_empty(), "used media must not be listed as orphan");

    // Cleanup removes the local record entirely.
    store
        .delete_media_upload("mxc://hs/cat")
        .await
        .expect("delete upload record");
    assert!(
        !store
            .media_upload_owned_by("mxc://hs/cat", "alice-key", "my-blog", "hello")
            .await
            .expect("ownership after delete"),
        "deleted upload must no longer prove ownership"
    );
}

#[tokio::test]
async fn media_upload_idempotency_replays_the_same_request() {
    let store = DbStore::connect(&test_db_url("media-upload-idempotency-replay"))
        .await
        .expect("connect db");
    let input = MediaUploadIdempotencyInput {
        key: "upload-key-123456".to_string(),
        request_fingerprint: "fp-1".to_string(),
    };

    let created = store
        .save_media_upload_idempotent("mxc://hs/first", "alice-key", "my-blog", "hello", &input)
        .await
        .expect("record first upload");
    assert!(matches!(
        created,
        MediaUploadIdempotencyOutcome::Created { mxc_url } if mxc_url == "mxc://hs/first"
    ));

    let replay = store
        .save_media_upload_idempotent("mxc://hs/second", "alice-key", "my-blog", "hello", &input)
        .await
        .expect("replay upload");
    assert!(matches!(
        replay,
        MediaUploadIdempotencyOutcome::Replayed { mxc_url } if mxc_url == "mxc://hs/first"
    ));
    assert!(
        !store
            .media_upload_owned_by("mxc://hs/second", "alice-key", "my-blog", "hello")
            .await
            .expect("ownership check"),
        "losing upload must be rolled back"
    );
    let found = store
        .find_media_upload_idempotency("alice-key", "upload-key-123456")
        .await
        .expect("find idempotency")
        .expect("record exists");
    assert_eq!(found.mxc_url, "mxc://hs/first");

    store
        .delete_media_upload("mxc://hs/first")
        .await
        .expect("delete upload record");
    assert!(
        store
            .find_media_upload_idempotency("alice-key", "upload-key-123456")
            .await
            .expect("find after delete")
            .is_none(),
        "deleting the upload must also drop its idempotency record"
    );
}

#[tokio::test]
async fn media_upload_idempotency_rejects_key_reuse_with_different_request() {
    let store = DbStore::connect(&test_db_url("media-upload-idempotency-reused"))
        .await
        .expect("connect db");

    store
        .save_media_upload_idempotent(
            "mxc://hs/first",
            "alice-key",
            "my-blog",
            "hello",
            &MediaUploadIdempotencyInput {
                key: "upload-key-123456".to_string(),
                request_fingerprint: "fp-1".to_string(),
            },
        )
        .await
        .expect("record first upload");

    let reused = store
        .save_media_upload_idempotent(
            "mxc://hs/second",
            "alice-key",
            "my-blog",
            "hello",
            &MediaUploadIdempotencyInput {
                key: "upload-key-123456".to_string(),
                request_fingerprint: "fp-2".to_string(),
            },
        )
        .await
        .expect("reuse check");
    assert_eq!(reused, MediaUploadIdempotencyOutcome::Reused);
    assert!(
        !store
            .media_upload_owned_by("mxc://hs/second", "alice-key", "my-blog", "hello")
            .await
            .expect("ownership check"),
        "reused request must not record a second upload"
    );
}

#[tokio::test]
async fn media_content_survives_roundtrip() {
    let store = DbStore::connect(&test_db_url("message-media"))
        .await
        .expect("connect db");
    let mut message = guest_message("$event:hs", "unused");
    message.content = Content::Media(MediaContent {
        kind: MediaKind::Image,
        url: "mxc://hs/abc".to_string(),
        filename: Some("cat.png".to_string()),
        mimetype: Some("image/png".to_string()),
        size: Some(1024),
        width: Some(100),
        height: Some(80),
        thumbnail_url: Some("mxc://hs/thumb".to_string()),
        alt_text: Some("a cat".to_string()),
        voice: false,
    });
    store.save_message(&message).await.expect("save message");

    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(
        stored.content,
        Content::Media(MediaContent {
            kind: MediaKind::Image,
            url: "mxc://hs/abc".to_string(),
            filename: Some("cat.png".to_string()),
            mimetype: Some("image/png".to_string()),
            size: Some(1024),
            width: Some(100),
            height: Some(80),
            thumbnail_url: Some("mxc://hs/thumb".to_string()),
            alt_text: Some("a cat".to_string()),
            voice: false,
        })
    );
}

#[tokio::test]
async fn virtual_user_mapping_is_stable_across_server_name_changes() {
    let store = DbStore::connect(&test_db_url("virtual-user-stable"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");

    let first = store
        .get_or_create_virtual_user("key1", &site, "hs")
        .await
        .expect("create virtual user");
    let second = store
        .get_or_create_virtual_user("key1", &site, "other.hs")
        .await
        .expect("reuse virtual user");

    assert_eq!(first, second);
    assert!(first.starts_with("@_cumments_my-blog_"));
    assert!(first.ends_with(":hs"));
}

use chrono::Utc;
use cumments_core::commands::{DeleteCommentCommand, PostCommentCommand, UpdateCommentCommand};
use cumments_core::media_upload::{MediaUploadIdempotencyInput, MediaUploadIdempotencyOutcome};
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, CommentMedia, Content, EditProjectionOutcome, MediaContent,
    MediaKind, Message, MessageRedactionOutcome, MessageRevision, MessageSaveOutcome,
    MessageStatus, PageSlug, PollContent, PollOption, PollResponseSummary, PollVote, Reaction,
    RoomMember, SiteId, SubmissionCompletion, TextContent, TextStyle, UnknownContent,
};
use cumments_core::ports::{
    AppServiceTxnStore, MessageStore, ProjectionSink, RoomStore, SubmissionStore, VirtualUserStore,
};
use cumments_store::DbStore;
use cumments_store::entities::{message_revisions, messages, poll_response_events};
use sea_orm::{Database, EntityTrait, QueryFilter};

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

fn visitor_message(event_id: &str, body: &str) -> Message {
    Message {
        event_id: event_id.to_string(),
        site_id: "my-blog".to_string(),
        page_slug: "hello".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Visitor,
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
        matrix_event_type: "m.room.message".to_string(),
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
        sender_mxid: "@_cumments_my-blog_a1b2c3d4e5f60718a1b2c3d4e5f60718:hs".to_string(),
        raw_content: serde_json::json!({ "msgtype": "m.text", "body": body }),
    }
}

async fn poll_counts(store: &DbStore) -> Vec<(i64, i64)> {
    let stored = store
        .get_message("$poll:hs")
        .await
        .expect("get poll")
        .expect("poll exists");
    match stored.content {
        Content::Poll(poll) => {
            let mut counts = poll
                .responses
                .into_iter()
                .map(|response| (response.option_index, response.count))
                .collect::<Vec<_>>();
            counts.sort_unstable();
            counts
        }
        other => panic!("expected poll content, got {other:?}"),
    }
}

#[tokio::test]
async fn save_message_records_typed_content_and_internal_fields() {
    let store = DbStore::connect(&test_db_url("message-sender"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PageSlug::from("hello");

    let parent = visitor_message("$parent:hs", "parent");
    store.save_message(&parent).await.expect("save parent");
    let message = visitor_message("$event:hs", "hello");
    store.save_message(&message).await.expect("save message");

    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(stored.room_id, "!room:hs");
    assert_eq!(
        stored.sender_mxid,
        "@_cumments_my-blog_a1b2c3d4e5f60718a1b2c3d4e5f60718:hs"
    );
    assert_eq!(stored.author.kind, AuthorKind::Visitor);
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
        .get_messages(&site, &slug, 10, 0, None)
        .await
        .expect("query messages");
    assert_eq!(page.total, 2);
    assert_eq!(page.items[0].event_id, "$event:hs");
}

#[tokio::test]
async fn author_profile_reads_live_member_state_and_falls_back_on_leave() {
    let store = DbStore::connect(&test_db_url("live-author-profile"))
        .await
        .expect("connect db");

    let message = visitor_message("$live:hs", "hello");
    store.save_message(&message).await.expect("save message");

    // No member row yet: the stored projection is the fallback.
    let stored = store
        .get_message("$live:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(stored.author.display_name.as_deref(), Some("Alice"));
    assert!(stored.author.avatar_url.is_none());

    // A joined member with a newer profile: reads follow it live.
    store
        .save_member(&RoomMember {
            room_id: message.room_id.clone(),
            user_id: message.sender_mxid.clone(),
            display_name: Some("新版名字".to_string()),
            avatar_url: Some("mxc://hs/new-avatar".to_string()),
            membership: "join".to_string(),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("save joined member");
    let live = store
        .get_message("$live:hs")
        .await
        .expect("get live message")
        .expect("message exists");
    assert_eq!(live.author.display_name.as_deref(), Some("新版名字"));
    assert_eq!(
        live.author.avatar_url.as_deref(),
        Some("mxc://hs/new-avatar")
    );
    let current_name = store
        .get_author_display_name("$live:hs")
        .await
        .expect("get current display name");
    assert_eq!(current_name.flatten().as_deref(), Some("新版名字"));

    // After leaving, the stored snapshot is the fallback again.
    store
        .save_member(&RoomMember {
            room_id: message.room_id,
            user_id: message.sender_mxid,
            display_name: None,
            avatar_url: None,
            membership: "leave".to_string(),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("save left member");
    let left = store
        .get_message("$live:hs")
        .await
        .expect("get left message")
        .expect("message exists");
    assert_eq!(left.author.display_name.as_deref(), Some("Alice"));
    assert!(left.author.avatar_url.is_none());
    let fallback_name = store
        .get_author_display_name("$live:hs")
        .await
        .expect("get fallback display name");
    assert_eq!(fallback_name.flatten().as_deref(), Some("Alice"));
}

#[tokio::test]
async fn apply_edit_updates_content_and_records_revision() {
    let store = DbStore::connect(&test_db_url("message-edit"))
        .await
        .expect("connect db");
    let parent = visitor_message("$parent:hs", "parent");
    store.save_message(&parent).await.expect("save parent");
    let message = visitor_message("$event:hs", "original");
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
        message_event_id: "$event:hs".to_string(),
        content: updated.content.clone(),
        edited_at: updated.edited_at.unwrap(),
        editor_mxid: "@_cumments_my-blog_a1b2c3d4e5f60718a1b2c3d4e5f60718:hs".to_string(),
        redacted_at: None,
    };

    assert_eq!(
        store
            .apply_edit(&updated, &revision)
            .await
            .expect("apply edit"),
        EditProjectionOutcome::AppliedCurrent
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
        message_event_id: "$event:hs".to_string(),
        content: updated.content.clone(),
        edited_at: stale_ts,
        editor_mxid: "someone".to_string(),
        redacted_at: None,
    };
    assert_eq!(
        store
            .apply_edit(&updated, &stale)
            .await
            .expect("stale edit stored"),
        EditProjectionOutcome::Superseded
    );

    // The stale replacement remains an immutable relation fact even though it
    // does not become the public view. A newer edit's redaction must be able
    // to select it later.
    let stored_stale = store
        .get_message_revision("$stale:hs")
        .await
        .expect("get stale revision")
        .expect("stale revision exists");
    assert_eq!(stored_stale.message_event_id, "$event:hs");
    assert!(stored_stale.redacted_at.is_none());
    let current = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert!(
        matches!(current.content, Content::Text(ref text) if text.body == "edited"),
        "a stale edit must not replace the current view"
    );
}

#[tokio::test]
async fn duplicate_original_projection_preserves_the_edited_view() {
    let store = DbStore::connect(&test_db_url("duplicate-original"))
        .await
        .expect("connect db");
    let message = visitor_message("$event:hs", "original");
    assert_eq!(
        store.save_message(&message).await.expect("save message"),
        MessageSaveOutcome::Inserted
    );
    let edited_at = Utc::now();
    let mut updated = message.clone();
    updated.content = Content::Text(TextContent {
        body: "edited".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    updated.edited_at = Some(edited_at);
    assert_eq!(
        store
            .apply_edit(
                &updated,
                &MessageRevision {
                    event_id: "$edit:hs".to_string(),
                    message_event_id: "$event:hs".to_string(),
                    content: updated.content.clone(),
                    edited_at,
                    editor_mxid: message.sender_mxid.clone(),
                    redacted_at: None,
                },
            )
            .await
            .expect("apply edit"),
        EditProjectionOutcome::AppliedCurrent
    );

    // A homeserver retry of the immutable original is a no-op; it must not
    // overwrite the derived current content with the pre-edit payload.
    assert_eq!(
        store.save_message(&message).await.expect("replay original"),
        MessageSaveOutcome::AlreadyProjected
    );
    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert!(
        matches!(stored.content, Content::Text(ref text) if text.body == "edited"),
        "original replay must not resurrect pre-edit content"
    );
}

#[tokio::test]
async fn redacting_the_latest_revision_rolls_back_to_an_older_revision() {
    let store = DbStore::connect(&test_db_url("revision-redact-older"))
        .await
        .expect("connect db");
    let message = visitor_message("$event:hs", "original");
    store.save_message(&message).await.expect("save message");

    let base = Utc::now();
    for (event_id, body, offset_secs) in [("$older:hs", "older", 0), ("$newer:hs", "newer", 2)] {
        let mut updated = message.clone();
        updated.content = Content::Text(TextContent {
            body: body.to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        });
        updated.edited_at = Some(base + chrono::Duration::seconds(offset_secs));
        assert_eq!(
            store
                .apply_edit(
                    &updated,
                    &MessageRevision {
                        event_id: event_id.to_string(),
                        message_event_id: "$event:hs".to_string(),
                        content: updated.content.clone(),
                        edited_at: updated.edited_at.unwrap(),
                        editor_mxid: message.sender_mxid.clone(),
                        redacted_at: None,
                    },
                )
                .await
                .unwrap_or_else(|error| panic!("apply {event_id}: {error:#}")),
            EditProjectionOutcome::AppliedCurrent,
            "{event_id} should become current"
        );
    }

    let now = Utc::now();
    assert!(
        store
            .redact_message_revision("$newer:hs", "!room:hs", now, "@moderator:hs")
            .await
            .expect("redact newer revision"),
    );
    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert!(
        matches!(stored.content, Content::Text(ref text) if text.body == "older"),
        "redacting the newest edit must reveal the older surviving edit"
    );
    assert_eq!(
        stored.edited_at.map(|at| at.timestamp_millis()),
        Some(base.timestamp_millis())
    );
}

#[tokio::test]
async fn redacting_the_only_revision_restores_the_original() {
    let url = test_db_url("revision-redact-only");
    let store = DbStore::connect(&url).await.expect("connect db");
    let message = visitor_message("$event:hs", "original");
    store.save_message(&message).await.expect("save message");
    let edited_at = Utc::now();
    let mut updated = message.clone();
    updated.content = Content::Text(TextContent {
        body: "edited".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    updated.edited_at = Some(edited_at);
    assert_eq!(
        store
            .apply_edit(
                &updated,
                &MessageRevision {
                    event_id: "$edit:hs".to_string(),
                    message_event_id: "$event:hs".to_string(),
                    content: updated.content.clone(),
                    edited_at,
                    editor_mxid: message.sender_mxid.clone(),
                    redacted_at: None,
                },
            )
            .await
            .expect("apply edit"),
        EditProjectionOutcome::AppliedCurrent
    );

    assert!(
        store
            .redact_message_revision("$edit:hs", "!room:hs", Utc::now(), "@moderator:hs")
            .await
            .expect("redact revision")
    );
    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert!(
        matches!(stored.content, Content::Text(ref text) if text.body == "original"),
        "redacting the only edit must restore the original"
    );
    assert!(stored.edited_at.is_none());

    let db = Database::connect(&url).await.expect("connect raw db");
    let revision = message_revisions::Entity::find()
        .filter(message_revisions::COLUMN.event_id.eq("$edit:hs"))
        .one(&db)
        .await
        .expect("query redacted revision")
        .expect("redacted revision metadata remains");
    assert!(revision.redacted_at.is_some());
    assert_eq!(
        revision.content_json, r#"{"type":"redacted"}"#,
        "a redacted replacement must not retain its authored payload"
    );
}

#[tokio::test]
async fn redact_message_rewrites_content_and_suppresses_metadata_and_aggregates() {
    let url = test_db_url("message-redact");
    let store = DbStore::connect(&url).await.expect("connect db");
    let message = visitor_message("$event:hs", "secret");
    store.save_message(&message).await.expect("save message");

    let edited_at = Utc::now();
    let mut updated = message.clone();
    updated.content = Content::Text(TextContent {
        body: "edited secret".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    updated.edited_at = Some(edited_at);
    assert_eq!(
        store
            .apply_edit(
                &updated,
                &MessageRevision {
                    event_id: "$edit:hs".to_string(),
                    message_event_id: "$event:hs".to_string(),
                    content: updated.content.clone(),
                    edited_at,
                    editor_mxid: message.sender_mxid.clone(),
                    redacted_at: None,
                },
            )
            .await
            .expect("apply edit"),
        EditProjectionOutcome::AppliedCurrent
    );
    store
        .save_reaction(&Reaction {
            event_id: "$reaction:hs".to_string(),
            message_event_id: "$event:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            key: "👍".to_string(),
            origin_server_ts: 1,
            redacted_at: None,
        })
        .await
        .expect("save reaction");

    let now = Utc::now();
    assert_eq!(
        store
            .redact_message("$event:hs", "!room:hs", now, ":hs")
            .await
            .expect("redact message"),
        MessageRedactionOutcome::Redacted
    );
    let stored = store
        .get_message("$event:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(stored.status, MessageStatus::Redacted);
    assert_eq!(stored.redacted_by.as_deref(), Some(":hs"));
    assert_eq!(stored.content, Content::Redacted);
    assert_eq!(stored.raw_content, serde_json::json!({}));
    assert!(stored.edited_at.is_none());
    assert!(stored.reply_to.is_none());
    assert!(stored.thread_root.is_none());
    assert!(stored.submission_id.is_none());
    assert!(stored.reactions.is_empty());

    let db = Database::connect(&url).await.expect("connect raw db");
    let row = messages::Entity::find()
        .filter(messages::COLUMN.event_id.eq("$event:hs"))
        .one(&db)
        .await
        .expect("query redacted row")
        .expect("redacted row exists");
    assert_eq!(row.original_content_json, r#"{"type":"redacted"}"#);
    let revisions = message_revisions::Entity::find()
        .filter(message_revisions::COLUMN.message_event_id.eq("$event:hs"))
        .all(&db)
        .await
        .expect("query revisions");
    assert!(
        revisions.is_empty(),
        "parent deletion must remove all revision payloads"
    );

    // A late or replayed replacement cannot restore deleted content.
    updated.edited_at = Some(Utc::now());
    assert_eq!(
        store
            .apply_edit(
                &updated,
                &MessageRevision {
                    event_id: "$late-edit:hs".to_string(),
                    message_event_id: "$event:hs".to_string(),
                    content: Content::Text(TextContent {
                        body: "restored".to_string(),
                        formatted_body: None,
                        style: TextStyle::Normal,
                    }),
                    edited_at: updated.edited_at.expect("edited at"),
                    editor_mxid: message.sender_mxid.clone(),
                    redacted_at: None,
                }
            )
            .await
            .expect("late edit rejected"),
        EditProjectionOutcome::Rejected
    );

    assert_eq!(
        store
            .redact_message("$missing:hs", "!room:hs", now, ":hs")
            .await
            .expect("missing target"),
        MessageRedactionOutcome::Rejected
    );
}

#[tokio::test]
async fn reactions_aggregate_by_key_and_ignore_redacted() {
    let store = DbStore::connect(&test_db_url("message-reactions"))
        .await
        .expect("connect db");
    let message = visitor_message("$event:hs", "hello");
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
async fn relations_to_redacted_parents_are_hidden_from_the_child_view() {
    let store = DbStore::connect(&test_db_url("child-dangling-relations"))
        .await
        .expect("connect db");
    let parent = visitor_message("$parent:hs", "parent");
    store.save_message(&parent).await.expect("save parent");

    let mut child = visitor_message("$child:hs", "reply");
    child.reply_to = Some("$parent:hs".to_string());
    child.thread_root = Some("$parent:hs".to_string());
    store.save_message(&child).await.expect("save child");

    assert!(child.reply_to.is_some());
    let visible = store
        .get_message("$child:hs")
        .await
        .expect("get child")
        .expect("child");
    assert_eq!(visible.reply_to.as_deref(), Some("$parent:hs"));

    assert_eq!(
        store
            .redact_message("$parent:hs", "!room:hs", Utc::now(), ":hs")
            .await
            .expect("redact parent"),
        MessageRedactionOutcome::Redacted
    );

    let visible = store
        .get_message("$child:hs")
        .await
        .expect("get child after parent redaction")
        .expect("child exists");
    assert_eq!(visible.status, MessageStatus::Active);
    assert!(visible.reply_to.is_none());
    assert!(visible.thread_root.is_none());
}

#[tokio::test]
async fn processed_appservice_transactions_are_durable_and_idempotent() {
    let store = DbStore::connect(&test_db_url("appservice-txn-dedupe"))
        .await
        .expect("connect db");

    assert!(
        !store
            .has_processed_txn("txn-1")
            .await
            .expect("query transaction"),
    );
    store.mark_processed_txn("txn-1").await.expect("mark txn");
    assert!(
        store
            .has_processed_txn("txn-1")
            .await
            .expect("query marked transaction"),
    );
}

#[tokio::test]
async fn poll_votes_aggregate_and_latest_vote_wins() {
    let store = DbStore::connect(&test_db_url("message-poll"))
        .await
        .expect("connect db");
    let mut message = visitor_message("$poll:hs", "poll placeholder");
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
        max_selections: 1,
        responses: Vec::new(),
        my_votes: Vec::new(),
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
            option_index: Some(0),
            origin_server_ts: 1,
        })
        .await
        .expect("alice votes");
    store
        .save_poll_vote(&PollVote {
            event_id: "$vote-bob:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@bob:hs".to_string(),
            option_index: Some(1),
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
            option_index: Some(1),
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
async fn redacting_the_latest_poll_response_restores_the_previous_vote() {
    let store = DbStore::connect(&test_db_url("poll-response-redact-rollback"))
        .await
        .expect("connect db");
    let mut message = visitor_message("$poll:hs", "poll placeholder");
    message.content = Content::Poll(PollContent {
        question: "best?".to_string(),
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
        max_selections: 1,
        responses: Vec::new(),
        my_votes: Vec::new(),
    });
    store.save_message(&message).await.expect("save poll");

    for (event_id, option_index, timestamp) in
        [("$alice-a:hs", Some(0), 1), ("$alice-b:hs", Some(1), 2)]
    {
        store
            .save_poll_vote(&PollVote {
                event_id: event_id.to_string(),
                poll_message_id: "$poll:hs".to_string(),
                sender_mxid: "@alice:hs".to_string(),
                option_index,
                origin_server_ts: timestamp,
            })
            .await
            .expect("save response");
    }

    assert_eq!(poll_counts(&store).await, [(1, 1)]);

    // Removing the newest relation must restore the previous valid vote.
    assert!(
        store
            .redact_poll_vote("$alice-b:hs", Utc::now(), ":hs")
            .await
            .expect("redact newest response")
    );
    assert_eq!(poll_counts(&store).await, [(0, 1)]);

    // A newer invalid or empty selection spoils the voter's previous choice.
    store
        .save_poll_vote(&PollVote {
            event_id: "$alice-spoiled:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            option_index: None,
            origin_server_ts: 3,
        })
        .await
        .expect("save spoiled response");
    assert_eq!(poll_counts(&store).await, []);

    // Redacting the spoiled response restores the prior valid vote.
    assert!(
        store
            .redact_poll_vote("$alice-spoiled:hs", Utc::now(), ":hs")
            .await
            .expect("redact spoiled response")
    );
    assert_eq!(poll_counts(&store).await, [(0, 1)]);
}

#[tokio::test]
async fn poll_selections_aggregate_per_option() {
    let store = DbStore::connect(&test_db_url("poll-multi-select"))
        .await
        .expect("connect db");
    let mut message = visitor_message("$poll:hs", "poll placeholder");
    message.content = Content::Poll(PollContent {
        question: "best?".to_string(),
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
        max_selections: 2,
        responses: Vec::new(),
        my_votes: Vec::new(),
    });
    store.save_message(&message).await.expect("save poll");

    let vote = |event_id: &str, sender: &str| PollVote {
        event_id: event_id.to_owned(),
        poll_message_id: "$poll:hs".to_owned(),
        sender_mxid: sender.to_owned(),
        option_index: None,
        origin_server_ts: 1,
    };
    let alice = vote("$alice:hs", "@alice:hs");
    let bob = vote("$bob:hs", "@bob:hs");
    store
        .save_poll_vote_with_selections(&alice, &["a".to_owned(), "b".to_owned()], None)
        .await
        .expect("alice votes twice");
    store
        .save_poll_vote_with_selections(&bob, &["b".to_owned()], None)
        .await
        .expect("bob votes once");

    let stored = store
        .get_message("$poll:hs")
        .await
        .expect("get poll")
        .expect("poll exists");
    let Content::Poll(poll) = stored.content else {
        panic!("expected poll");
    };
    assert_eq!(
        poll.responses,
        vec![
            PollResponseSummary {
                option_index: 0,
                count: 1
            },
            PollResponseSummary {
                option_index: 1,
                count: 2
            },
        ]
    );
}

#[tokio::test]
async fn redacted_poll_votes_leave_the_aggregate_and_do_not_resurrect() {
    let url = test_db_url("message-poll-redact");
    let store = DbStore::connect(&url).await.expect("connect db");
    let mut message = visitor_message("$poll:hs", "poll placeholder");
    message.content = Content::Poll(PollContent {
        question: "best? ".to_string(),
        options: vec![PollOption {
            id: "a".to_string(),
            text: "A".to_string(),
        }],
        max_selections: 1,
        responses: Vec::new(),
        my_votes: Vec::new(),
    });
    store
        .save_message(&message)
        .await
        .expect("save poll message");

    let bob_vote = PollVote {
        event_id: "$vote-bob:hs".to_string(),
        poll_message_id: "$poll:hs".to_string(),
        sender_mxid: "@bob:hs".to_string(),
        option_index: Some(0),
        origin_server_ts: 2,
    };
    store.save_poll_vote(&bob_vote).await.expect("bob votes");
    store
        .save_poll_vote(&PollVote {
            event_id: "$vote-alice:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            option_index: Some(0),
            origin_server_ts: 1,
        })
        .await
        .expect("alice votes");

    assert!(
        store
            .redact_poll_vote("$vote-bob:hs", Utc::now(), ":hs")
            .await
            .expect("redact bob vote")
    );
    let db = Database::connect(&url).await.expect("connect raw db");
    let redacted = poll_response_events::Entity::find()
        .filter(poll_response_events::COLUMN.event_id.eq("$vote-bob:hs"))
        .one(&db)
        .await
        .expect("query redacted vote")
        .expect("redacted vote exists");
    assert!(redacted.redacted_at.is_some());
    assert_eq!(
        redacted.option_index, None,
        "redaction must forget the selected option"
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
    let mut message = visitor_message("$poll:hs", "poll placeholder");
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
        max_selections: 1,
        responses: Vec::new(),
        my_votes: Vec::new(),
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
            option_index: Some(1),
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
            option_index: Some(0),
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
    let mut message = visitor_message("$event:hs", "unused");
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
        .record_media_upload("mxc://hs/cat", "alice-key", "my-blog", Some("hello"))
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
        .record_media_upload("mxc://hs/cat", "alice-key", "my-blog", Some("hello"))
        .await
        .expect("re-record upload");

    let site_urls = store
        .list_media_urls_for_site("my-blog")
        .await
        .expect("list site media");
    assert_eq!(site_urls, vec!["mxc://hs/cat".to_string()]);
    assert!(
        store
            .list_media_urls_for_site("other-blog")
            .await
            .expect("other site media")
            .is_empty()
    );

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
async fn orphan_sweep_skips_media_bound_to_a_retrying_submission() {
    let store = DbStore::connect(&test_db_url("media-submission"))
        .await
        .expect("connect db");
    store
        .record_media_upload("mxc://hs/cat", "alice-key", "my-blog", Some("hello"))
        .await
        .expect("record upload");

    let command = PostCommentCommand {
        site_id: SiteId::from("my-blog"),
        page_slug: PageSlug::from("hello"),
        content: "with media".to_string(),
        media: Some(CommentMedia {
            kind: Some(MediaKind::Image),
            url: "mxc://hs/cat".to_string(),
            filename: Some("cat.png".to_string()),
            mimetype: Some("image/png".to_string()),
            size: Some(42),
            width: Some(64),
            height: Some(64),
            voice: false,
        }),
        location: None,
        poll: None,
        display_name: "Alice".to_string(),
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "sig".to_string(),
        author_challenge: "chal".to_string(),
        reply_to: None,
        thread_root: None,
    };
    let id = store
        .save_post_submission(&command)
        .await
        .expect("save submission");

    let orphan_cutoff = Utc::now() + chrono::Duration::days(1);
    assert!(
        store
            .list_unused_media_before(orphan_cutoff)
            .await
            .expect("list unused")
            .is_empty(),
        "media referenced by a pending submission must not be swept"
    );

    store
        .dead_letter_post_submission(id, "terminal")
        .await
        .expect("dead letter");
    assert_eq!(
        store
            .list_unused_media_before(orphan_cutoff)
            .await
            .expect("list unused after terminal")
            .len(),
        1,
        "once the submission is terminal the media is orphan-eligible again"
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
        .save_media_upload_idempotent(
            "mxc://hs/first",
            "alice-key",
            "my-blog",
            Some("hello"),
            &input,
        )
        .await
        .expect("record first upload");
    assert!(matches!(
        created,
        MediaUploadIdempotencyOutcome::Created { mxc_url } if mxc_url == "mxc://hs/first"
    ));

    let replay = store
        .save_media_upload_idempotent(
            "mxc://hs/second",
            "alice-key",
            "my-blog",
            Some("hello"),
            &input,
        )
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
            Some("hello"),
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
            Some("hello"),
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
    let mut message = visitor_message("$event:hs", "unused");
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

#[tokio::test]
async fn projection_sink_closes_a_post_after_fact_only_replay() {
    let store = DbStore::connect(&test_db_url("sink-post-replay"))
        .await
        .expect("connect db");
    let command_id = store
        .save_post_submission(&PostCommentCommand {
            site_id: SiteId::from("my-blog"),
            page_slug: PageSlug::from("hello"),
            content: "hello".to_string(),
            media: None,
            location: None,
            poll: None,
            display_name: "Alice".to_string(),
            author_public_key: "key".to_string(),
            author_signature: "signature".to_string(),
            author_challenge: "challenge".to_string(),
            reply_to: None,
            thread_root: None,
        })
        .await
        .expect("save post");
    let message = visitor_message("$event:hs", "hello");

    // Simulate the old crash window: the fact committed, but closure did not.
    store.save_message(&message).await.expect("save fact");
    let outcome = store
        .save_message_unit(&message, SubmissionCompletion::PostById(command_id))
        .await
        .expect("replay projection");
    assert_eq!(outcome, MessageSaveOutcome::AlreadyProjected);
    assert!(
        store
            .claim_pending_post_submissions(10, Utc::now())
            .await
            .expect("claim posts")
            .is_empty()
    );
}

#[tokio::test]
async fn projection_sink_closes_an_edit_after_fact_only_replay() {
    let store = DbStore::connect(&test_db_url("sink-edit-replay"))
        .await
        .expect("connect db");
    let parent = visitor_message("$parent:hs", "parent");
    let message = visitor_message("$event:hs", "original");
    store.save_message(&parent).await.expect("save parent");
    store.save_message(&message).await.expect("save message");
    let submission_id = store
        .save_update_submission(&UpdateCommentCommand {
            site_id: SiteId::from("my-blog"),
            page_slug: PageSlug::from("hello"),
            event_id: "$event:hs".to_string(),
            content: "edited".to_string(),
            author_public_key: "key".to_string(),
            author_signature: "signature".to_string(),
            author_challenge: "challenge".to_string(),
        })
        .await
        .expect("save update");

    let edited_at = Utc::now();
    let mut updated = message.clone();
    updated.content = Content::Text(TextContent {
        body: "edited".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    updated.edited_at = Some(edited_at);
    let revision = MessageRevision {
        event_id: "$edit:hs".to_string(),
        message_event_id: "$event:hs".to_string(),
        content: updated.content.clone(),
        edited_at,
        editor_mxid: "@alice:hs".to_string(),
        redacted_at: None,
    };
    assert_eq!(
        store.apply_edit(&updated, &revision).await.expect("apply"),
        EditProjectionOutcome::AppliedCurrent
    );
    assert_eq!(
        store
            .apply_edit_unit(
                &updated,
                &revision,
                SubmissionCompletion::UpdateById(submission_id)
            )
            .await
            .expect("replay edit"),
        EditProjectionOutcome::AlreadyKnown
    );
    assert!(
        store
            .claim_pending_update_submissions(10, Utc::now())
            .await
            .expect("claim updates")
            .is_empty()
    );
}

#[tokio::test]
async fn projection_sink_closes_a_delete_after_redaction_replay() {
    let store = DbStore::connect(&test_db_url("sink-delete-replay"))
        .await
        .expect("connect db");
    let message = visitor_message("$event:hs", "delete me");
    store.save_message(&message).await.expect("save message");
    let _submission_id = store
        .save_delete_submission(&DeleteCommentCommand {
            site_id: SiteId::from("my-blog"),
            page_slug: PageSlug::from("hello"),
            event_id: "$event:hs".to_string(),
            author_public_key: "key".to_string(),
            author_signature: "signature".to_string(),
            author_challenge: "challenge".to_string(),
        })
        .await
        .expect("save delete");

    let redacted_at = Utc::now();
    assert_eq!(
        store
            .redact_message("$event:hs", "!room:hs", redacted_at, "@alice:hs")
            .await
            .expect("redact"),
        MessageRedactionOutcome::Redacted
    );
    assert_eq!(
        store
            .redact_message_unit(
                "$event:hs",
                "!room:hs",
                redacted_at,
                "@alice:hs",
                "$redaction:hs"
            )
            .await
            .expect("replay delete"),
        MessageRedactionOutcome::AlreadyRedacted
    );
    assert!(
        store
            .has_backfill_tombstone("$event:hs", "!room:hs")
            .await
            .expect("tombstone")
    );
    assert!(
        store
            .claim_pending_delete_submissions(10, Utc::now())
            .await
            .expect("claim deletes")
            .is_empty()
    );
}

#[tokio::test]
async fn projection_sink_redaction_sanitizes_retained_payloads() {
    let url = test_db_url("sink-redact-payloads");
    let store = DbStore::connect(&url).await.expect("connect db");
    let message = visitor_message("$event:hs", "original secret");
    store.save_message(&message).await.expect("save message");

    let edited_at = Utc::now();
    let mut updated = message.clone();
    updated.content = Content::Text(TextContent {
        body: "edited secret".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    updated.edited_at = Some(edited_at);
    assert_eq!(
        store
            .apply_edit(
                &updated,
                &MessageRevision {
                    event_id: "$edit:hs".to_string(),
                    message_event_id: "$event:hs".to_string(),
                    content: updated.content.clone(),
                    edited_at,
                    editor_mxid: message.sender_mxid.clone(),
                    redacted_at: None,
                },
            )
            .await
            .expect("apply edit"),
        EditProjectionOutcome::AppliedCurrent
    );

    let redacted_at = Utc::now();
    assert_eq!(
        store
            .redact_message_unit(
                "$event:hs",
                "!room:hs",
                redacted_at,
                "@moderator:hs",
                "$redaction:hs"
            )
            .await
            .expect("redact message"),
        MessageRedactionOutcome::Redacted
    );

    let db = Database::connect(&url).await.expect("connect raw db");
    let row = messages::Entity::find()
        .filter(messages::COLUMN.event_id.eq("$event:hs"))
        .one(&db)
        .await
        .expect("query redacted row")
        .expect("redacted row exists");
    assert_eq!(row.content_json, r#"{"type":"redacted"}"#);
    assert_eq!(row.original_content_json, r#"{"type":"redacted"}"#);
    assert_eq!(row.raw_content_json, "{}");

    let revisions = message_revisions::Entity::find()
        .filter(message_revisions::COLUMN.message_event_id.eq("$event:hs"))
        .all(&db)
        .await
        .expect("query revisions");
    assert!(
        revisions.is_empty(),
        "parent deletion must remove all revision payloads"
    );
}

#[tokio::test]
async fn poll_my_votes_batch_personalization_and_latest_wins() {
    let store = DbStore::connect(&test_db_url("poll-my-votes"))
        .await
        .expect("connect db");

    // Two polls on same page, to test batch query (N+1 check)
    let mut poll1 = visitor_message("$poll1:hs", "poll1");
    poll1.content = Content::Poll(PollContent {
        question: "best?".to_string(),
        options: vec![
            PollOption {
                id: "0".to_string(),
                text: "A".to_string(),
            },
            PollOption {
                id: "1".to_string(),
                text: "B".to_string(),
            },
        ],
        max_selections: 1,
        responses: Vec::new(),
        my_votes: Vec::new(),
    });
    let mut poll2 = visitor_message("$poll2:hs", "poll2");
    poll2.content = Content::Poll(PollContent {
        question: "other?".to_string(),
        options: vec![
            PollOption {
                id: "0".to_string(),
                text: "X".to_string(),
            },
            PollOption {
                id: "1".to_string(),
                text: "Y".to_string(),
            },
        ],
        max_selections: 1,
        responses: Vec::new(),
        my_votes: Vec::new(),
    });
    store.save_message(&poll1).await.expect("save poll1");
    store.save_message(&poll2).await.expect("save poll2");
    // Register rooms for active check (messages are already active)
    // Viewer identification: no proof -> empty (tested via unknown sender)
    let empty = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@unknown:hs")
        .await
        .expect("query unknown");
    assert!(empty.is_empty(), "unknown voter should have no my_votes");

    // Alice has no vote yet -> empty, even though she could have Matrix activity
    let alice_no_vote = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@alice:hs")
        .await
        .expect("alice no vote");
    assert!(alice_no_vote.is_empty());

    // 4. Alice votes A on poll1, Bob not voted
    store
        .save_poll_vote_with_selections(
            &PollVote {
                event_id: "$vote-alice-1:hs".to_string(),
                poll_message_id: "$poll1:hs".to_string(),
                sender_mxid: "@alice:hs".to_string(),
                option_index: Some(0),
                origin_server_ts: 10,
            },
            &["0".to_string()],
            None,
        )
        .await
        .expect("alice vote A");
    let alice_votes = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@alice:hs")
        .await
        .expect("alice votes");
    assert_eq!(
        alice_votes.get("$poll1:hs").cloned().unwrap_or_default(),
        vec!["0".to_string()]
    );
    let bob_votes = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@bob:hs")
        .await
        .expect("bob votes");
    assert!(bob_votes.is_empty(), "Bob should have []");

    // Ensure public aggregate unchanged by my_votes logic: poll1 should have 1 vote for option 0
    let stored = store
        .get_message("$poll1:hs")
        .await
        .expect("get poll1")
        .expect("exists");
    match stored.content {
        Content::Poll(p) => {
            assert_eq!(p.responses.len(), 1);
            assert_eq!(p.responses[0].option_index, 0);
            assert_eq!(p.responses[0].count, 1);
        }
        _ => panic!("expected poll"),
    }

    // 5. Alice changes vote from A -> B (latest wins)
    store
        .save_poll_vote_with_selections(
            &PollVote {
                event_id: "$vote-alice-2:hs".to_string(),
                poll_message_id: "$poll1:hs".to_string(),
                sender_mxid: "@alice:hs".to_string(),
                option_index: Some(1),
                origin_server_ts: 20,
            },
            &["1".to_string()],
            None,
        )
        .await
        .expect("alice vote B");
    let alice_b = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@alice:hs")
        .await
        .expect("alice B");
    assert_eq!(
        alice_b.get("$poll1:hs").cloned().unwrap_or_default(),
        vec!["1".to_string()]
    );
    // Aggregate should now be B count 1, not A
    let stored2 = store
        .get_message("$poll1:hs")
        .await
        .expect("get poll1")
        .expect("exists");
    match stored2.content {
        Content::Poll(p) => {
            assert_eq!(p.responses.len(), 1);
            assert_eq!(p.responses[0].option_index, 1);
        }
        _ => panic!("expected poll"),
    }

    // 6. Redaction restore: redact Alice's latest B, should restore A
    store
        .redact_poll_vote("$vote-alice-2:hs", Utc::now(), "@alice:hs")
        .await
        .expect("redact B");
    let alice_after_redact = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@alice:hs")
        .await
        .expect("after redact");
    assert_eq!(
        alice_after_redact
            .get("$poll1:hs")
            .cloned()
            .unwrap_or_default(),
        vec!["0".to_string()],
        "redacting latest should restore previous A"
    );
    // Aggregate also restores A
    let stored3 = store
        .get_message("$poll1:hs")
        .await
        .expect("get poll1")
        .expect("exists");
    match stored3.content {
        Content::Poll(p) => {
            assert_eq!(p.responses[0].option_index, 0);
        }
        _ => panic!("expected poll"),
    }

    // 7. Spoiled/unvote: latest response with empty selections and spoiled_reason
    store
        .save_poll_vote_with_selections(
            &PollVote {
                event_id: "$vote-alice-spoiled:hs".to_string(),
                poll_message_id: "$poll1:hs".to_string(),
                sender_mxid: "@alice:hs".to_string(),
                option_index: None,
                origin_server_ts: 30,
            },
            &[],
            Some("unknown_answer"),
        )
        .await
        .expect("spoiled");
    let alice_spoiled = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@alice:hs")
        .await
        .expect("spoiled");
    assert!(
        !alice_spoiled.contains_key("$poll1:hs") || alice_spoiled["$poll1:hs"].is_empty(),
        "spoiled should be []"
    );
    // Aggregate should have 0 responses (no valid vote)
    let stored4 = store
        .get_message("$poll1:hs")
        .await
        .expect("get poll1")
        .expect("exists");
    match stored4.content {
        Content::Poll(p) => {
            assert!(
                p.responses.is_empty(),
                "spoiled vote should produce empty responses"
            );
        }
        _ => panic!("expected poll"),
    }
    // Redacting spoiled restores A again
    store
        .redact_poll_vote("$vote-alice-spoiled:hs", Utc::now(), "@alice:hs")
        .await
        .expect("redact spoiled");
    let alice_restored = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@alice:hs")
        .await
        .expect("restored");
    assert_eq!(
        alice_restored.get("$poll1:hs").cloned().unwrap_or_default(),
        vec!["0".to_string()]
    );

    // 8. Public aggregate not changed by my_votes: ensure my_votes does not affect other voters
    // Bob votes B on poll1, Alice's my_votes still A, aggregate should have 2 voters (A and B)
    // First restore: ensure Alice is A, then Bob votes B
    // (Alice currently A after redact of spoiled)
    store
        .save_poll_vote_with_selections(
            &PollVote {
                event_id: "$vote-bob-1:hs".to_string(),
                poll_message_id: "$poll1:hs".to_string(),
                sender_mxid: "@bob:hs".to_string(),
                option_index: Some(1),
                origin_server_ts: 40,
            },
            &["1".to_string()],
            None,
        )
        .await
        .expect("bob vote B");
    let stored5 = store
        .get_message("$poll1:hs")
        .await
        .expect("get poll1")
        .expect("exists");
    match stored5.content {
        Content::Poll(p) => {
            // Should have 2 distinct voters, each counting once
            assert_eq!(p.responses.len(), 2);
            let counts: std::collections::HashMap<i64, i64> = p
                .responses
                .into_iter()
                .map(|r| (r.option_index, r.count))
                .collect();
            assert_eq!(counts.get(&0), Some(&1));
            assert_eq!(counts.get(&1), Some(&1));
        }
        _ => panic!("expected poll"),
    }
    // Alice's my_votes still A, not affected by Bob
    let alice_still_a = store
        .find_poll_my_votes(&["$poll1:hs".to_string()], "@alice:hs")
        .await
        .expect("alice still");
    assert_eq!(
        alice_still_a.get("$poll1:hs").cloned().unwrap_or_default(),
        vec!["0".to_string()]
    );

    // Batch test: Alice votes on poll2 as well, batch query returns both
    store
        .save_poll_vote_with_selections(
            &PollVote {
                event_id: "$vote-alice-poll2:hs".to_string(),
                poll_message_id: "$poll2:hs".to_string(),
                sender_mxid: "@alice:hs".to_string(),
                option_index: Some(1),
                origin_server_ts: 50,
            },
            &["1".to_string()],
            None,
        )
        .await
        .expect("alice poll2");
    let batch = store
        .find_poll_my_votes(
            &["$poll1:hs".to_string(), "$poll2:hs".to_string()],
            "@alice:hs",
        )
        .await
        .expect("batch");
    assert_eq!(
        batch.get("$poll1:hs").cloned().unwrap_or_default(),
        vec!["0".to_string()]
    );
    assert_eq!(
        batch.get("$poll2:hs").cloned().unwrap_or_default(),
        vec!["1".to_string()]
    );
    // Ensure batch is single query (indirectly verified by not doing N+1, but functional test covers correctness)
}
#[tokio::test]
async fn thread_query_filters_replies_and_supports_pagination() {
    let store = DbStore::connect(&test_db_url("thread-query"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PageSlug::from("hello");

    // Helper to create a message with explicit thread_root/reply_to/status
    fn make_message(
        event_id: &str,
        thread_root: Option<String>,
        reply_to: Option<String>,
        status: MessageStatus,
        ts: chrono::DateTime<chrono::Utc>,
    ) -> Message {
        let mut msg = visitor_message(event_id, event_id);
        msg.thread_root = thread_root;
        msg.reply_to = reply_to;
        msg.status = status;
        msg.timestamp = ts;
        msg.redacted_at = if status == MessageStatus::Redacted {
            Some(chrono::Utc::now())
        } else {
            None
        };
        msg.redacted_by = if status == MessageStatus::Redacted {
            Some("@mod:hs".to_string())
        } else {
            None
        };
        msg
    }

    let base = chrono::Utc::now();
    // Root is a top-level comment (thread_root = None)
    let root = make_message(
        "$root:hs",
        None,
        None,
        MessageStatus::Active,
        base + chrono::Duration::seconds(1),
    );
    let reply1 = make_message(
        "$reply1:hs",
        Some("$root:hs".to_string()),
        Some("$root:hs".to_string()),
        MessageStatus::Active,
        base + chrono::Duration::seconds(2),
    );
    // Nested reply shares the same thread_root
    let reply2 = make_message(
        "$reply2:hs",
        Some("$root:hs".to_string()),
        Some("$reply1:hs".to_string()),
        MessageStatus::Active,
        base + chrono::Duration::seconds(3),
    );
    let other_root = make_message(
        "$other_root:hs",
        None,
        None,
        MessageStatus::Active,
        base + chrono::Duration::seconds(4),
    );
    let other_reply = make_message(
        "$other_reply:hs",
        Some("$other_root:hs".to_string()),
        Some("$other_root:hs".to_string()),
        MessageStatus::Active,
        base + chrono::Duration::seconds(5),
    );
    let top = make_message(
        "$top:hs",
        None,
        None,
        MessageStatus::Active,
        base + chrono::Duration::seconds(6),
    );
    // A redacted reply that should never be returned in thread queries
    let redacted = make_message(
        "$redacted:hs",
        Some("$root:hs".to_string()),
        Some("$root:hs".to_string()),
        MessageStatus::Redacted,
        base + chrono::Duration::seconds(7),
    );

    for msg in [
        &root,
        &reply1,
        &reply2,
        &other_root,
        &other_reply,
        &top,
        &redacted,
    ] {
        store.save_message(msg).await.expect("save message");
    }

    // 1. Without thread_root, existing page-level behavior is unchanged:
    // total counts all rows (including redacted as tombstones) and pagination still works.
    // Our get_messages without filter currently returns all rows regardless of status,
    // so total should be 7.
    let page_all = store
        .get_messages(&site, &slug, 10, 0, None)
        .await
        .expect("query all");
    assert_eq!(
        page_all.total, 7,
        "page-level total must remain unchanged (includes tombstones)"
    );
    assert_eq!(page_all.items.len(), 7);

    // 2. With thread_root = $root, only active replies in that thread are returned.
    // Root itself (thread_root NULL) must not be returned.
    // Other thread's reply must not be returned. Nested reply must be included.
    // Redacted reply must not be counted.
    let thread = store
        .get_messages(&site, &slug, 10, 0, Some("$root:hs"))
        .await
        .expect("thread query");
    assert_eq!(thread.total, 2, "thread total must be active replies only");
    assert_eq!(thread.items.len(), 2);
    let ids: std::collections::HashSet<String> =
        thread.items.iter().map(|m| m.event_id.clone()).collect();
    assert!(ids.contains("$reply1:hs"), "direct reply must be included");
    assert!(
        ids.contains("$reply2:hs"),
        "nested reply must be included (same thread_root)"
    );
    assert!(
        !ids.contains("$root:hs"),
        "root itself must not be returned"
    );
    assert!(
        !ids.contains("$other_reply:hs"),
        "other thread must not be included"
    );
    assert!(
        !ids.contains("$top:hs"),
        "top-level unrelated must not be included"
    );
    assert!(
        !ids.contains("$redacted:hs"),
        "redacted reply must not be counted"
    );

    // 6. total is active reply count, not page-local count.
    // 7. pagination works within the thread.
    let page1 = store
        .get_messages(&site, &slug, 1, 0, Some("$root:hs"))
        .await
        .expect("thread page1");
    assert_eq!(page1.total, 2);
    assert_eq!(page1.items.len(), 1);
    let page2 = store
        .get_messages(&site, &slug, 1, 1, Some("$root:hs"))
        .await
        .expect("thread page2");
    assert_eq!(page2.total, 2);
    assert_eq!(page2.items.len(), 1);
    assert_ne!(page1.items[0].event_id, page2.items[0].event_id);
    // Ensure pagination covers both replies.
    let mut all_ids = std::collections::HashSet::new();
    all_ids.insert(page1.items[0].event_id.clone());
    all_ids.insert(page2.items[0].event_id.clone());
    assert_eq!(all_ids, ids);

    // 8. deleted/redacted reply does not appear and total decreases.
    store
        .redact_message("$reply1:hs", "!room:hs", chrono::Utc::now(), "@mod:hs")
        .await
        .expect("redact reply1");
    let after_redact = store
        .get_messages(&site, &slug, 10, 0, Some("$root:hs"))
        .await
        .expect("thread after redact");
    assert_eq!(
        after_redact.total, 1,
        "redacted reply must not be counted in total"
    );
    assert_eq!(after_redact.items.len(), 1);
    assert_eq!(after_redact.items[0].event_id, "$reply2:hs");

    // Verify page-level still returns tombstone (behavior unchanged).
    let page_all_after = store
        .get_messages(&site, &slug, 10, 0, None)
        .await
        .expect("page all after redact");
    // page-level still includes all rows (including the newly redacted one as tombstone)
    assert_eq!(page_all_after.total, 7);
    assert!(
        page_all_after
            .items
            .iter()
            .any(|m| m.event_id == "$reply1:hs" && m.status == MessageStatus::Redacted)
    );
}

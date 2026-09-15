use chrono::Utc;
use cumments_core::commands::{DeleteCommentCommand, PostCommentCommand, UpdateCommentCommand};
use cumments_core::media_upload::{MediaUploadIdempotencyInput, MediaUploadIdempotencyOutcome};
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, CommentMedia, Content, EditProjectionOutcome, MediaContent,
    MediaKind, Message, MessageRedactionOutcome, MessageRevision, MessageSaveOutcome,
    MessageStatus, PageSlug, PollContent, PollEnd, PollOption, PollResponseSummary, PollVote,
    Reaction, RoomMember, SiteId, SubmissionCompletion, TextContent, TextStyle, ThreadSummary,
    UnknownContent,
};
use cumments_core::poll::{PollSemanticKind, PollStatus};
use cumments_core::ports::{
    AppServiceTxnStore, MessageStore, ProjectionSink, RoomStore, SubmissionStore, VirtualUserStore,
};
use cumments_store::DbStore;
use cumments_store::entities::{message_revisions, messages, poll_response_events};
use sea_orm::{Database, EntityTrait, QueryFilter};

fn make_test_poll(question: &str, answers: Vec<(&str, &str)>, max_selections: u64) -> PollContent {
    PollContent {
        question: question.to_string(),
        answers: answers
            .into_iter()
            .map(|(id, text)| PollOption {
                id: id.to_string(),
                text: text.to_string(),
            })
            .collect(),
        kind: PollSemanticKind::Disclosed,
        max_selections,
        status: PollStatus::Open,
        end_time: None,
        results: None,
        total_votes: 0,
        responses: Vec::new(),
        my_votes: None,
    }
}

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

        thread_summary: None,
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
            origin_server_ts: 1000,
            event_id: Some("$join".to_string()),
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
            origin_server_ts: 2000,
            event_id: Some("$leave".to_string()),
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
async fn redact_message_rewrites_content_and_suppresses_content_metadata() {
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
async fn redaction_preserves_relation_metadata_on_the_redacted_message() {
    let url = test_db_url("redact-preserves-relations");
    let store = DbStore::connect(&url).await.expect("connect db");
    let root = visitor_message("$root:hs", "root");
    store.save_message(&root).await.expect("save root");
    let parent = visitor_message("$parent:hs", "parent");
    store.save_message(&parent).await.expect("save parent");

    let mut member = visitor_message("$member:hs", "secret");
    member.reply_to = Some("$parent:hs".to_string());
    member.thread_root = Some("$root:hs".to_string());
    store.save_message(&member).await.expect("save member");

    store
        .redact_message("$member:hs", "!room:hs", Utc::now(), "@mod:hs")
        .await
        .expect("redact member");

    let stored = store
        .get_message("$member:hs")
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(stored.status, MessageStatus::Redacted);
    assert_eq!(stored.content, Content::Redacted);
    assert_eq!(
        stored.reply_to.as_deref(),
        Some("$parent:hs"),
        "redacted hydration must preserve the stored reply_to"
    );
    assert_eq!(
        stored.thread_root.as_deref(),
        Some("$root:hs"),
        "redacted hydration must preserve the stored thread_root"
    );

    let db = Database::connect(&url).await.expect("connect raw db");
    let row = messages::Entity::find()
        .filter(messages::COLUMN.event_id.eq("$member:hs"))
        .one(&db)
        .await
        .expect("query redacted row")
        .expect("redacted row exists");
    assert_eq!(
        row.reply_to.as_deref(),
        Some("$parent:hs"),
        "redaction mutation must not erase reply_to"
    );
    assert_eq!(
        row.thread_root.as_deref(),
        Some("$root:hs"),
        "redaction mutation must not erase thread_root"
    );
}

#[tokio::test]
async fn redacted_root_keeps_its_thread_queryable() {
    let store = DbStore::connect(&test_db_url("redacted-root-thread"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PageSlug::from("hello");

    let mut root = visitor_message("$root:hs", "root");
    root.reply_to = None;
    store.save_message(&root).await.expect("save root");

    let mut member_a = visitor_message("$a:hs", "a");
    member_a.reply_to = Some("$root:hs".to_string());
    member_a.thread_root = Some("$root:hs".to_string());
    store.save_message(&member_a).await.expect("save member a");

    let mut member_b = visitor_message("$b:hs", "b");
    member_b.reply_to = None;
    member_b.thread_root = Some("$root:hs".to_string());
    store.save_message(&member_b).await.expect("save member b");

    assert_eq!(
        store
            .redact_message("$root:hs", "!room:hs", Utc::now(), "@mod:hs")
            .await
            .expect("redact root"),
        MessageRedactionOutcome::Redacted
    );

    // Root redaction does not delete members: the Thread collection still
    // returns the active members of the still-identified Thread.
    let thread = store
        .get_messages(&site, &slug, 10, 0, Some("$root:hs"))
        .await
        .expect("thread query");
    assert_eq!(thread.total, 2);
    let ids: std::collections::HashSet<String> =
        thread.items.iter().map(|m| m.event_id.clone()).collect();
    assert_eq!(ids, ["$a:hs".to_string(), "$b:hs".to_string()].into());
    for member in &thread.items {
        assert_eq!(
            member.thread_root.as_deref(),
            Some("$root:hs"),
            "members of a redacted root keep their thread relation in the public view"
        );
    }

    // The redacted root remains individually addressable.
    let stored_root = store
        .get_message("$root:hs")
        .await
        .expect("get root")
        .expect("root exists");
    assert_eq!(stored_root.status, MessageStatus::Redacted);
    assert!(
        stored_root.thread_root.is_none(),
        "the root is the Thread identity, never a member of its own Thread"
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
async fn redacted_parent_hides_reply_to_but_keeps_thread_membership_visible() {
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
    assert!(
        visible.reply_to.is_none(),
        "reply edges to redacted parents stay hidden from the child view"
    );
    assert_eq!(
        visible.thread_root.as_deref(),
        Some("$parent:hs"),
        "thread membership survives the root's redaction: the root event ID remains the Thread identity"
    );
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
    message.content = Content::Poll(make_test_poll("best? ", vec![("a", "A"), ("b", "B")], 1));
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
    message.content = Content::Poll(make_test_poll("best?", vec![("a", "A"), ("b", "B")], 1));
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
    message.content = Content::Poll(make_test_poll("best?", vec![("a", "A"), ("b", "B")], 2));
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
    message.content = Content::Poll(make_test_poll("best? ", vec![("a", "A")], 1));
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
    message.content = Content::Poll(make_test_poll("best? ", vec![("a", "A"), ("b", "B")], 1));
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

    // Candidate enumeration is anchored on `created_at` and ignores `used_at`.
    let candidates = store
        .list_media_upload_candidates_before(Utc::now() + chrono::Duration::days(1))
        .await
        .expect("list candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].mxc_url, "mxc://hs/cat");
    assert_eq!(candidates[0].site_id, "my-blog");

    store
        .mark_media_used("my-blog", "mxc://hs/cat")
        .await
        .expect("mark used");
    let candidates_after_use = store
        .list_media_upload_candidates_before(Utc::now() + chrono::Duration::days(1))
        .await
        .expect("list candidates after use");
    assert_eq!(
        candidates_after_use.len(),
        1,
        "used_at is historical bookkeeping and must not gate candidacy"
    );

    // Releasing ownership removes only the local ownership evidence.
    let cat_id = store
        .get_media_upload("my-blog", "mxc://hs/cat")
        .await
        .unwrap()
        .expect("upload exists")
        .id;
    assert!(
        store
            .release_media_upload_ownership("my-blog", "mxc://hs/cat", cat_id)
            .await
            .expect("release ownership")
    );
    assert!(
        !store
            .media_upload_owned_by("mxc://hs/cat", "alice-key", "my-blog", "hello")
            .await
            .expect("ownership after release"),
        "released upload must no longer prove ownership"
    );
}

#[tokio::test]
async fn media_upload_ownership_is_site_scoped_for_the_same_mxc() {
    let store = DbStore::connect(&test_db_url("media-uploads-site-scoped"))
        .await
        .expect("connect db");

    let mxc = "mxc://hs/shared";
    let site_a = "site-a";
    let site_b = "site-b";

    // Both sites may own the same MXC independently.
    store
        .record_media_upload(mxc, "author-a", site_a, Some("post-a"))
        .await
        .unwrap();
    store
        .record_media_upload(mxc, "author-b", site_b, Some("post-b"))
        .await
        .unwrap();

    let a = store
        .get_media_upload(site_a, mxc)
        .await
        .unwrap()
        .expect("site-a row exists");
    let b = store
        .get_media_upload(site_b, mxc)
        .await
        .unwrap()
        .expect("site-b row exists");
    assert_ne!(a.id, b.id, "each site has its own ownership row");
    assert_eq!(a.site_id, site_a);
    assert_eq!(a.author_public_key, "author-a");
    assert_eq!(b.site_id, site_b);
    assert_eq!(b.author_public_key, "author-b");

    // Recording site-b must not overwrite site-a's ownership.
    assert!(store.has_media_upload_for_site(site_a, mxc).await.unwrap());
    assert!(store.has_media_upload_for_site(site_b, mxc).await.unwrap());
    assert!(
        store
            .media_upload_owned_by(mxc, "author-a", site_a, "post-a")
            .await
            .unwrap()
    );
    assert!(
        !store
            .media_upload_owned_by(mxc, "author-b", site_a, "post-b")
            .await
            .unwrap(),
        "site-a lookup must not see site-b's ownership"
    );

    // A site with no row returns nothing.
    assert!(
        store
            .get_media_upload("site-c", mxc)
            .await
            .unwrap()
            .is_none()
    );

    // Re-recording updates only that site's row, keeping its id.
    store
        .record_media_upload(mxc, "author-a2", site_a, Some("post-a2"))
        .await
        .unwrap();
    let a_after = store.get_media_upload(site_a, mxc).await.unwrap().unwrap();
    let b_after = store.get_media_upload(site_b, mxc).await.unwrap().unwrap();
    assert_eq!(a_after.id, a.id, "same-site recording must keep the row id");
    assert_eq!(a_after.author_public_key, "author-a2");
    assert_eq!(b_after.id, b.id, "site-b's row must be untouched");
    assert_eq!(b_after.author_public_key, "author-b");

    // Releasing site-a's row leaves site-b's row intact.
    assert!(
        store
            .release_media_upload_ownership(site_a, mxc, a.id)
            .await
            .unwrap()
    );
    assert!(store.get_media_upload(site_a, mxc).await.unwrap().is_none());
    let b_after_release = store
        .get_media_upload(site_b, mxc)
        .await
        .unwrap()
        .expect("site-b row must survive");
    assert_eq!(b_after_release.id, b.id);
}

#[tokio::test]
async fn mark_media_used_is_site_scoped() {
    let store = DbStore::connect(&test_db_url("media-uploads-mark-used"))
        .await
        .expect("connect db");

    let mxc = "mxc://hs/shared-used";
    store
        .record_media_upload(mxc, "author-a", "site-a", None)
        .await
        .unwrap();
    store
        .record_media_upload(mxc, "author-b", "site-b", None)
        .await
        .unwrap();

    store.mark_media_used("site-a", mxc).await.unwrap();

    let a = store
        .get_media_upload("site-a", mxc)
        .await
        .unwrap()
        .unwrap();
    let b = store
        .get_media_upload("site-b", mxc)
        .await
        .unwrap()
        .unwrap();
    assert!(a.used_at.is_some(), "site-a's row must be marked used");
    assert!(
        b.used_at.is_none(),
        "marking site-a must not mutate site-b's row"
    );

    // Marking a site with no matching row is a no-op.
    store.mark_media_used("site-c", mxc).await.unwrap();
    assert!(
        store
            .get_media_upload("site-b", mxc)
            .await
            .unwrap()
            .unwrap()
            .used_at
            .is_none()
    );
}

#[tokio::test]
async fn media_upload_idempotency_is_site_scoped_for_the_same_mxc() {
    let store = DbStore::connect(&test_db_url("media-uploads-idempotency-sites"))
        .await
        .expect("connect db");

    let mxc = "mxc://hs/idem-shared";
    let author = "author-1";

    let created_a = store
        .save_media_upload_idempotent(
            mxc,
            author,
            "site-a",
            Some("page-a"),
            &MediaUploadIdempotencyInput {
                key: "key-a".to_string(),
                request_fingerprint: "fp-a".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        created_a,
        MediaUploadIdempotencyOutcome::Created { .. }
    ));

    let created_b = store
        .save_media_upload_idempotent(
            mxc,
            author,
            "site-b",
            Some("page-b"),
            &MediaUploadIdempotencyInput {
                key: "key-b".to_string(),
                request_fingerprint: "fp-b".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(created_b, MediaUploadIdempotencyOutcome::Created { .. }),
        "site-b must be able to record the same MXC independently"
    );

    let a = store
        .get_media_upload("site-a", mxc)
        .await
        .unwrap()
        .unwrap();
    let b = store
        .get_media_upload("site-b", mxc)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.site_id, "site-a");
    assert_eq!(b.site_id, "site-b");
    assert_eq!(a.page_slug.as_deref(), Some("page-a"));
    assert_eq!(b.page_slug.as_deref(), Some("page-b"));

    // Replaying site-a's request must not mutate site-b's ownership row.
    let replay_a = store
        .save_media_upload_idempotent(
            mxc,
            author,
            "site-a",
            Some("page-a"),
            &MediaUploadIdempotencyInput {
                key: "key-a".to_string(),
                request_fingerprint: "fp-a".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        replay_a,
        MediaUploadIdempotencyOutcome::Replayed { .. }
    ));
    let b_after = store
        .get_media_upload("site-b", mxc)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b_after.id, b.id);
    assert_eq!(b_after.page_slug.as_deref(), Some("page-b"));

    // Idempotency records stay independent per key.
    assert!(
        store
            .find_media_upload_idempotency(author, "key-a")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .find_media_upload_idempotency(author, "key-b")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn media_reachability_store_queries_operate_correctly() {
    let store = DbStore::connect(&test_db_url("media-reachability-store"))
        .await
        .expect("connect db");

    let site_a = "site-alpha";
    let site_b = "site-beta";
    let mxc_1 = "mxc://hs/upload-1";
    let mxc_2 = "mxc://hs/upload-2";
    let mxc_b = "mxc://hs/upload-b";

    // 1. Test get_media_upload and list_media_uploads_for_site
    assert!(
        store
            .get_media_upload(site_a, mxc_1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .list_media_uploads_for_site(site_a)
            .await
            .unwrap()
            .is_empty()
    );

    store
        .record_media_upload(mxc_1, "pubkey-1", site_a, Some("slug-1"))
        .await
        .unwrap();
    store
        .record_media_upload(mxc_2, "pubkey-2", site_a, None)
        .await
        .unwrap();
    store
        .record_media_upload(mxc_b, "pubkey-b", site_b, None)
        .await
        .unwrap();

    let upload_1 = store
        .get_media_upload(site_a, mxc_1)
        .await
        .unwrap()
        .expect("upload 1 exists");
    assert_eq!(upload_1.mxc_url, mxc_1);
    assert_eq!(upload_1.author_public_key, "pubkey-1");
    assert_eq!(upload_1.site_id, site_a);
    assert_eq!(upload_1.page_slug.as_deref(), Some("slug-1"));

    // Cross-site check: site_b querying mxc_1 returns None
    assert!(
        store
            .get_media_upload(site_b, mxc_1)
            .await
            .unwrap()
            .is_none()
    );

    let site_a_uploads = store.list_media_uploads_for_site(site_a).await.unwrap();
    assert_eq!(site_a_uploads.len(), 2);
    let site_b_uploads = store.list_media_uploads_for_site(site_b).await.unwrap();
    assert_eq!(site_b_uploads.len(), 1);

    // 2. Test has_historical_author_avatar
    let hist_mxc_a = "mxc://hs/hist-avatar-a";
    let hist_mxc_b = "mxc://hs/hist-avatar-b";

    assert!(
        !store
            .has_historical_author_avatar(site_a, hist_mxc_a)
            .await
            .unwrap()
    );

    // Insert message on site_a carrying hist_mxc_a as the historical author avatar
    let mut msg_a = visitor_message("$msg_hist_a", "hist msg a");
    msg_a.site_id = site_a.to_string();
    msg_a.author.avatar_url = Some(hist_mxc_a.to_string());
    store.save_message(&msg_a).await.unwrap();

    // Insert message on site_b carrying hist_mxc_b as the historical author avatar
    let mut msg_b = visitor_message("$msg_hist_b", "hist msg b");
    msg_b.site_id = site_b.to_string();
    msg_b.author.avatar_url = Some(hist_mxc_b.to_string());
    store.save_message(&msg_b).await.unwrap();

    assert!(
        store
            .has_historical_author_avatar(site_a, hist_mxc_a)
            .await
            .unwrap()
    );
    // Cross-site: site_a does NOT have site_b's historical avatar
    assert!(
        !store
            .has_historical_author_avatar(site_a, hist_mxc_b)
            .await
            .unwrap()
    );
    // site_b has hist_mxc_b
    assert!(
        store
            .has_historical_author_avatar(site_b, hist_mxc_b)
            .await
            .unwrap()
    );

    // 3. Test has_content_attachment
    let content_mxc_a = "mxc://hs/attachment-a";
    let content_mxc_b = "mxc://hs/attachment-b";

    assert!(
        !store
            .has_content_attachment(site_a, content_mxc_a)
            .await
            .unwrap()
    );

    let mut content_msg_a = visitor_message("$msg_content_a", "content a");
    content_msg_a.site_id = site_a.to_string();
    content_msg_a.content = Content::Media(MediaContent {
        kind: MediaKind::Image,
        url: content_mxc_a.to_string(),
        filename: None,
        mimetype: None,
        size: None,
        width: None,
        height: None,
        thumbnail_url: None,
        alt_text: None,
        voice: false,
    });
    store.save_message(&content_msg_a).await.unwrap();

    assert!(
        store
            .has_content_attachment(site_a, content_mxc_a)
            .await
            .unwrap()
    );
    // Cross-site: content on site_a does not make it present on site_b
    assert!(
        !store
            .has_content_attachment(site_b, content_mxc_a)
            .await
            .unwrap()
    );
    assert!(
        !store
            .has_content_attachment(site_a, content_mxc_b)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn media_upload_ownership_release_removes_exact_record_and_isolates_sites() {
    let store = DbStore::connect(&test_db_url("media-upload-release-isolation"))
        .await
        .expect("connect db");

    let site_a = "site-alpha";
    let site_b = "site-beta";
    let mxc_a1 = "mxc://hs/upload-a1";
    let mxc_a2 = "mxc://hs/upload-a2";
    let mxc_b1 = "mxc://hs/upload-b1";

    // Seed media_uploads with distinct records:
    // site_a has two uploads from different authors
    // site_b has one upload
    store
        .record_media_upload(mxc_a1, "author-pubkey-1", site_a, Some("post-1"))
        .await
        .unwrap();
    store
        .record_media_upload(mxc_a2, "author-pubkey-2", site_a, None)
        .await
        .unwrap();
    store
        .record_media_upload(mxc_b1, "author-pubkey-3", site_b, None)
        .await
        .unwrap();

    // Verify all exist
    assert!(
        store
            .get_media_upload(site_a, mxc_a1)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_media_upload(site_a, mxc_a2)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_media_upload(site_b, mxc_b1)
            .await
            .unwrap()
            .is_some()
    );

    // Capture the enumerated identities the release primitive must match.
    let a1_id = store
        .get_media_upload(site_a, mxc_a1)
        .await
        .unwrap()
        .unwrap()
        .id;
    let a2_id = store
        .get_media_upload(site_a, mxc_a2)
        .await
        .unwrap()
        .unwrap()
        .id;
    let b1_id = store
        .get_media_upload(site_b, mxc_b1)
        .await
        .unwrap()
        .unwrap()
        .id;

    // 1. Cross-site release safety: the id matches site_b's record, but the
    // expected site does not, so nothing may be deleted.
    let released_wrong_site = store
        .release_media_upload_ownership(site_a, mxc_b1, b1_id)
        .await
        .unwrap();
    assert!(
        !released_wrong_site,
        "cross-site release must return false (no rows affected)"
    );
    assert!(
        store
            .get_media_upload(site_b, mxc_b1)
            .await
            .unwrap()
            .is_some(),
        "site_b's record must not be affected by release on site_a"
    );

    // 2. Exact row deletion: releasing mxc_a1 removes only mxc_a1 on site_a
    let released_a1 = store
        .release_media_upload_ownership(site_a, mxc_a1, a1_id)
        .await
        .unwrap();
    assert!(released_a1, "releasing an existing record must return true");

    // Exactly that row is gone
    assert!(
        store
            .get_media_upload(site_a, mxc_a1)
            .await
            .unwrap()
            .is_none(),
        "released upload must no longer be found in media_uploads"
    );

    // Unrelated upload on same site from different author remains intact
    let upload_a2 = store
        .get_media_upload(site_a, mxc_a2)
        .await
        .unwrap()
        .expect("unrelated upload must remain");
    assert_eq!(upload_a2.author_public_key, "author-pubkey-2");
    assert_eq!(upload_a2.mxc_url, mxc_a2);

    // Unrelated upload on other site remains intact
    let upload_b1 = store
        .get_media_upload(site_b, mxc_b1)
        .await
        .unwrap()
        .expect("other site upload must remain");
    assert_eq!(upload_b1.author_public_key, "author-pubkey-3");

    // 3. Idempotent / harmless execution: releasing an already-missing row with
    // its stale identity is harmless.
    let released_a1_again = store
        .release_media_upload_ownership(site_a, mxc_a1, a1_id)
        .await
        .unwrap();
    assert!(
        !released_a1_again,
        "subsequent release must safely return false without error"
    );

    let released_nonexistent = store
        .release_media_upload_ownership(site_a, "mxc://hs/does-not-exist", i64::MAX)
        .await
        .unwrap();
    assert!(
        !released_nonexistent,
        "releasing nonexistent media must return false without error"
    );

    // 4. Test alias release_media_upload behaves identically
    let released_a2 = store
        .release_media_upload(site_a, mxc_a2, a2_id)
        .await
        .unwrap();
    assert!(
        released_a2,
        "alias release_media_upload must release the row"
    );
    assert!(
        store
            .get_media_upload(site_a, mxc_a2)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn media_upload_ownership_release_is_bound_to_the_enumerated_record() {
    let store = DbStore::connect(&test_db_url("media-upload-release-identity"))
        .await
        .expect("connect db");

    let site = "site-alpha";
    let mxc = "mxc://hs/recycled-mxc";

    store
        .record_media_upload(mxc, "author-1", site, Some("post-1"))
        .await
        .unwrap();
    let evaluated_id = store
        .get_media_upload(site, mxc)
        .await
        .unwrap()
        .expect("enumerated row exists")
        .id;

    // The exact recorded identity is released.
    assert!(
        store
            .release_media_upload_ownership(site, mxc, evaluated_id)
            .await
            .unwrap(),
        "the matching enumerated row must be released"
    );

    // Race shape: the evaluated row disappears, then a different row takes over
    // the same logical identity before the release primitive runs again.
    store
        .record_media_upload(mxc, "author-2", site, Some("post-2"))
        .await
        .unwrap();
    let replacement = store
        .get_media_upload(site, mxc)
        .await
        .unwrap()
        .expect("replacement row exists");
    assert_ne!(replacement.id, evaluated_id);

    assert!(
        !store
            .release_media_upload_ownership(site, mxc, evaluated_id)
            .await
            .unwrap(),
        "a stale id must not release a different row"
    );
    assert!(
        store.get_media_upload(site, mxc).await.unwrap().is_some(),
        "the replacement row must survive a stale release"
    );

    // The correct id under the wrong site must not reach the row either.
    assert!(
        !store
            .release_media_upload_ownership("site-beta", mxc, replacement.id)
            .await
            .unwrap(),
        "a mismatched site must not release the row"
    );
    assert!(
        store.get_media_upload(site, mxc).await.unwrap().is_some(),
        "the row must survive a release attempted under a different site"
    );
}

#[tokio::test]
async fn media_upload_ownership_release_preserves_idempotency() {
    let store = DbStore::connect(&test_db_url("media-upload-release-idempotency"))
        .await
        .expect("connect db");

    let site_id = "site-prod";
    let mxc_url = "mxc://hs/idempotent-asset";
    let author_key = "author-ed25519-key";
    let idem_key = "client-idem-key-999";

    // 1. Create upload via idempotent flow
    let input = MediaUploadIdempotencyInput {
        key: idem_key.to_string(),
        request_fingerprint: "fingerprint-abc".to_string(),
    };

    let outcome = store
        .save_media_upload_idempotent(mxc_url, author_key, site_id, Some("page-slug-1"), &input)
        .await
        .expect("save idempotent upload");
    assert!(matches!(
        outcome,
        MediaUploadIdempotencyOutcome::Created { .. }
    ));

    // Verify ownership row and idempotency row both exist
    assert!(
        store
            .get_media_upload(site_id, mxc_url)
            .await
            .unwrap()
            .is_some()
    );
    let idem_record = store
        .find_media_upload_idempotency(author_key, idem_key)
        .await
        .unwrap()
        .expect("idempotency record must exist");
    assert_eq!(idem_record.mxc_url, mxc_url);

    // 3. Explicitly release media upload ownership, bound to the enumerated row.
    let upload_id = store
        .get_media_upload(site_id, mxc_url)
        .await
        .unwrap()
        .expect("ownership row exists")
        .id;
    let released = store
        .release_media_upload_ownership(site_id, mxc_url, upload_id)
        .await
        .unwrap();
    assert!(released, "ownership record must be released");

    // Ownership row is removed
    assert!(
        store
            .get_media_upload(site_id, mxc_url)
            .await
            .unwrap()
            .is_none(),
        "ownership row must be gone"
    );

    // 4. CRITICAL: Idempotency record MUST remain unchanged and replayable
    let idem_after = store
        .find_media_upload_idempotency(author_key, idem_key)
        .await
        .unwrap()
        .expect("idempotency record MUST remain intact after ownership release");
    assert_eq!(idem_after.mxc_url, mxc_url);
    assert_eq!(idem_after.request_fingerprint, "fingerprint-abc");
}

#[tokio::test]
async fn media_upload_ownership_release_preserves_submission_integrity() {
    let url = test_db_url("media-upload-release-submission");
    let store = DbStore::connect(&url).await.expect("connect db");

    let site_id = "my-blog";
    let page_slug = "hello";
    let mxc_url = "mxc://hs/sub-media";

    // Record upload
    store
        .record_media_upload(mxc_url, "author-key", site_id, Some(page_slug))
        .await
        .unwrap();

    let command = PostCommentCommand {
        site_id: SiteId::from(site_id),
        page_slug: PageSlug::from(page_slug),
        content: "with media".to_string(),
        media: Some(CommentMedia {
            kind: Some(MediaKind::Image),
            url: mxc_url.to_string(),
            filename: Some("cat.png".to_string()),
            mimetype: Some("image/png".to_string()),
            size: Some(42),
            width: Some(64),
            height: Some(64),
            voice: false,
        }),
        location: None,
        poll: None,
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "sig".to_string(),
        author_challenge: "chal".to_string(),
        reply_to: None,
        thread_root: None,
    };
    let submission_id = store
        .save_post_submission(&command)
        .await
        .expect("save submission");

    let before = store
        .get_media_upload(site_id, mxc_url)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.submission_id, Some(submission_id));

    // Release ownership does not fail on submission-bound row
    let released = store
        .release_media_upload_ownership(site_id, mxc_url, before.id)
        .await
        .unwrap();
    assert!(released, "must release row successfully");
    assert!(
        store
            .get_media_upload(site_id, mxc_url)
            .await
            .unwrap()
            .is_none()
    );

    // The submission row itself remains completely unaffected in the store
    let pending = store
        .claim_pending_post_submissions(10, chrono::Utc::now() + chrono::Duration::minutes(5))
        .await
        .unwrap();
    assert!(pending.iter().any(|s| s.id == submission_id));
}

#[tokio::test]
async fn media_upload_candidates_are_age_scoped_and_submission_agnostic() {
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
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "sig".to_string(),
        author_challenge: "chal".to_string(),
        reply_to: None,
        thread_root: None,
    };
    store
        .save_post_submission(&command)
        .await
        .expect("save submission");

    // A freshly recorded upload is too young to be a candidate.
    let now = Utc::now();
    assert!(
        store
            .list_media_upload_candidates_before(now - chrono::Duration::hours(24))
            .await
            .expect("list candidates")
            .is_empty(),
        "uploads inside the grace period must not be candidates"
    );

    // Older than the cutoff it is enumerated regardless of any submission
    // binding; active-submission protection belongs to the reachability evaluator.
    let candidates = store
        .list_media_upload_candidates_before(now)
        .await
        .expect("list candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].mxc_url, "mxc://hs/cat");
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

    // Releasing ownership must not disturb the idempotency record.
    let first_id = store
        .get_media_upload("my-blog", "mxc://hs/first")
        .await
        .unwrap()
        .expect("ownership row exists")
        .id;
    assert!(
        store
            .release_media_upload_ownership("my-blog", "mxc://hs/first", first_id)
            .await
            .expect("release ownership")
    );
    assert!(
        store
            .find_media_upload_idempotency("alice-key", "upload-key-123456")
            .await
            .expect("find after release")
            .is_some(),
        "ownership release must preserve the idempotency record"
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
    poll1.content = Content::Poll(make_test_poll("best?", vec![("0", "A"), ("1", "B")], 1));
    let mut poll2 = visitor_message("$poll2:hs", "poll2");
    poll2.content = Content::Poll(make_test_poll("other?", vec![("0", "X"), ("1", "Y")], 1));
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

#[tokio::test]
async fn thread_summary_is_derived_for_roots_and_absent_for_members() {
    let store = DbStore::connect(&test_db_url("summary-placement"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PageSlug::from("hello");

    let mut root = visitor_message("$root:hs", "root");
    root.reply_to = None;
    store.save_message(&root).await.expect("save root");

    let empty_root = store
        .get_message("$root:hs")
        .await
        .expect("get root")
        .expect("root exists");
    assert_eq!(
        empty_root.thread_summary,
        Some(ThreadSummary::default()),
        "a root without members exposes the zero/empty summary"
    );

    let mut member = visitor_message("$member:hs", "member");
    member.reply_to = None;
    member.thread_root = Some("$root:hs".to_string());
    store.save_message(&member).await.expect("save member");

    let stored_root = store
        .get_message("$root:hs")
        .await
        .expect("get root")
        .expect("root exists");
    assert_eq!(
        stored_root.thread_summary,
        Some(ThreadSummary {
            num_replies: 1,
            latest_reply: Some("$member:hs".to_string())
        })
    );

    let stored_member = store
        .get_message("$member:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(
        stored_member.thread_summary, None,
        "a Thread member never carries a ThreadSummary merely because it belongs to a Thread"
    );

    // Collection and single GET expose the same derived summaries.
    let page = store
        .get_messages(&site, &slug, 10, 0, None)
        .await
        .expect("page query");
    let mut items = page.items;
    items.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].event_id, "$member:hs");
    assert_eq!(items[0].thread_summary, None);
    assert_eq!(items[1].event_id, "$root:hs");
    assert_eq!(
        items[1].thread_summary,
        Some(ThreadSummary {
            num_replies: 1,
            latest_reply: Some("$member:hs".to_string())
        })
    );
}

#[tokio::test]
async fn thread_summary_counts_active_members_and_picks_latest_by_canonical_order() {
    let store = DbStore::connect(&test_db_url("summary-ordering"))
        .await
        .expect("connect db");

    let mut root = visitor_message("$root:hs", "root");
    root.reply_to = None;
    store.save_message(&root).await.expect("save root");

    let base = Utc::now();
    // Direct reply to the root.
    let mut a = visitor_message("$a:hs", "a");
    a.reply_to = Some("$root:hs".to_string());
    a.thread_root = Some("$root:hs".to_string());
    a.timestamp = base + chrono::Duration::seconds(2);
    store.save_message(&a).await.expect("save a");

    // Member without a direct reply: contributes to the summary even though
    // it has no reply_to relation.
    let mut b = visitor_message("$b:hs", "b");
    b.reply_to = None;
    b.thread_root = Some("$root:hs".to_string());
    b.timestamp = base + chrono::Duration::seconds(3);
    store.save_message(&b).await.expect("save b");

    // Nested member sharing B's timestamp: canonical ordering
    // (timestamp DESC, event_id ASC) prefers the lexicographically smaller
    // event ID on ties.
    let mut c = visitor_message("$c:hs", "c");
    c.reply_to = Some("$a:hs".to_string());
    c.thread_root = Some("$root:hs".to_string());
    c.timestamp = base + chrono::Duration::seconds(3);
    store.save_message(&c).await.expect("save c");

    let stored_root = store
        .get_message("$root:hs")
        .await
        .expect("get root")
        .expect("root exists");
    assert_eq!(
        stored_root.thread_summary,
        Some(ThreadSummary {
            num_replies: 3,
            latest_reply: Some("$b:hs".to_string())
        }),
        "count covers nested members and members without reply_to; latest is the canonical first member"
    );
}

#[tokio::test]
async fn thread_summary_excludes_redacted_members_and_recomputes_latest() {
    let store = DbStore::connect(&test_db_url("summary-redaction"))
        .await
        .expect("connect db");

    let mut root = visitor_message("$root:hs", "root");
    root.reply_to = None;
    store.save_message(&root).await.expect("save root");

    let base = Utc::now();
    let mut a = visitor_message("$a:hs", "a");
    a.reply_to = Some("$root:hs".to_string());
    a.thread_root = Some("$root:hs".to_string());
    a.timestamp = base + chrono::Duration::seconds(2);
    store.save_message(&a).await.expect("save a");

    let mut b = visitor_message("$b:hs", "b");
    b.reply_to = None;
    b.thread_root = Some("$root:hs".to_string());
    b.timestamp = base + chrono::Duration::seconds(3);
    store.save_message(&b).await.expect("save b");

    async fn summary_of(store: &DbStore) -> Option<ThreadSummary> {
        store
            .get_message("$root:hs")
            .await
            .expect("get root")
            .expect("root exists")
            .thread_summary
    }
    assert_eq!(
        summary_of(&store).await,
        Some(ThreadSummary {
            num_replies: 2,
            latest_reply: Some("$b:hs".to_string())
        })
    );

    // Redacting the latest member removes it from the count and promotes the
    // previous member; a stale latest_reply can never survive.
    store
        .redact_message("$b:hs", "!room:hs", Utc::now(), "@mod:hs")
        .await
        .expect("redact b");
    assert_eq!(
        summary_of(&store).await,
        Some(ThreadSummary {
            num_replies: 1,
            latest_reply: Some("$a:hs".to_string())
        })
    );

    store
        .redact_message("$a:hs", "!room:hs", Utc::now(), "@mod:hs")
        .await
        .expect("redact a");
    assert_eq!(
        summary_of(&store).await,
        Some(ThreadSummary::default()),
        "no active members left means zero/null"
    );
}

#[tokio::test]
async fn thread_summary_matches_thread_collection_total() {
    let store = DbStore::connect(&test_db_url("summary-collection"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PageSlug::from("hello");

    let mut root = visitor_message("$root:hs", "root");
    root.reply_to = None;
    store.save_message(&root).await.expect("save root");

    let mut other_root = visitor_message("$other_root:hs", "other");
    other_root.reply_to = None;
    other_root.timestamp = Utc::now() + chrono::Duration::seconds(9);
    store.save_message(&other_root).await.expect("save other");

    let base = Utc::now();
    for (event_id, ts) in [("$m1:hs", 1), ("$m2:hs", 2)] {
        let mut member = visitor_message(event_id, event_id);
        member.reply_to = None;
        member.thread_root = Some("$root:hs".to_string());
        member.timestamp = base + chrono::Duration::seconds(ts);
        store.save_message(&member).await.expect("save member");
    }
    let mut other_member = visitor_message("$other_member:hs", "other member");
    other_member.reply_to = None;
    other_member.thread_root = Some("$other_root:hs".to_string());
    other_member.timestamp = base + chrono::Duration::seconds(5);
    store
        .save_message(&other_member)
        .await
        .expect("save other member");

    // A redacted root stays the basis for aggregation: its summary keeps
    // counting active members even though the root itself is inactive.
    store
        .redact_message("$other_root:hs", "!room:hs", Utc::now(), "@mod:hs")
        .await
        .expect("redact other root");

    // One collection read attaches correct summaries to every root in the
    // page (batch aggregation, no per-root query).
    let page = store
        .get_messages(&site, &slug, 10, 0, None)
        .await
        .expect("page query");
    let summary_of = |id: &str| {
        page.items
            .iter()
            .find(|m| m.event_id == id)
            .expect("root in page")
            .thread_summary
            .clone()
            .expect("roots expose a summary")
    };
    assert_eq!(summary_of("$root:hs").num_replies, 2);
    assert_eq!(
        summary_of("$root:hs").latest_reply,
        Some("$m2:hs".to_string())
    );
    assert_eq!(summary_of("$other_root:hs").num_replies, 1);

    // Consistency: num_replies equals the Thread collection total for the
    // same root (same active predicate, same membership key).
    let thread = store
        .get_messages(&site, &slug, 10, 0, Some("$root:hs"))
        .await
        .expect("thread query");
    assert_eq!(thread.total, summary_of("$root:hs").num_replies);
}

// ── Poll end reduction ────────────────────────────────────────────

/// A projected poll message with the given wire `kind` and declared answers.
fn poll_message(event_id: &str, kind: &str, max_selections: u8) -> Message {
    let mut message = visitor_message(event_id, "poll placeholder");
    message.reply_to = None;
    message.thread_root = None;
    let semantic_kind = match kind {
        "org.matrix.msc3381.poll.disclosed" => PollSemanticKind::Disclosed,
        _ => PollSemanticKind::Undisclosed,
    };
    message.content = Content::Poll(PollContent {
        question: "best?".to_string(),
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
        kind: semantic_kind,
        max_selections: u64::from(max_selections),
        status: PollStatus::Open,
        end_time: None,
        results: None,
        total_votes: 0,
        responses: Vec::new(),
        my_votes: None,
    });
    message.raw_content = serde_json::json!({
        "org.matrix.msc3381.poll.start": {
            "kind": kind,
            "question": { "org.matrix.msc1767.text": "best?" },
            "answers": [
                { "id": "a", "org.matrix.msc1767.text": "A" },
                { "id": "b", "org.matrix.msc1767.text": "B" },
            ],
        }
    });
    message
}

/// The sender of [`visitor_message`] and therefore the poll creator in these
/// tests; the reducer authorizes an end only from the poll's own sender.
const POLL_CREATOR: &str = "@_cumments_my-blog_a1b2c3d4e5f60718a1b2c3d4e5f60718:hs";

/// A poll end fact from `sender`. Authorization is not stored: the reducer
/// derives it from the poll creator, so only [`POLL_CREATOR`] ends close polls.
fn poll_end(event_id: &str, sender: &str, ts: i64) -> PollEnd {
    PollEnd {
        event_id: event_id.to_string(),
        poll_message_id: "$poll:hs".to_string(),
        sender_mxid: sender.to_string(),
        origin_server_ts: ts,
    }
}

#[tokio::test]
async fn poll_ends_reduce_to_the_earliest_authorized_end() {
    let store = DbStore::connect(&test_db_url("poll-end-earliest"))
        .await
        .expect("connect db");
    store
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            1,
        ))
        .await
        .expect("save poll");

    // An earlier unauthorized end must not preempt the later valid one.
    store
        .save_poll_end(&poll_end("$bad:hs", "@mallory:hs", 100))
        .await
        .expect("save unauthorized end");
    store
        .save_poll_end(&poll_end("$good:hs", POLL_CREATOR, 200))
        .await
        .expect("save authorized end");
    store
        .save_poll_end(&poll_end("$later:hs", POLL_CREATOR, 300))
        .await
        .expect("save later end");

    let projection = store
        .poll_projection("$poll:hs")
        .await
        .expect("derive projection")
        .expect("poll exists");
    assert_eq!(projection.status, PollStatus::Ended);
    let effective = projection.end.expect("effective end");
    assert_eq!(effective.event_id, "$good:hs");
    assert_eq!(effective.origin_server_ts, 200);
}

#[tokio::test]
async fn unauthorized_poll_ends_do_not_close_the_poll() {
    let store = DbStore::connect(&test_db_url("poll-end-unauthorized"))
        .await
        .expect("connect db");
    store
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            1,
        ))
        .await
        .expect("save poll");
    store
        .save_poll_end(&poll_end("$bad:hs", "@mallory:hs", 100))
        .await
        .expect("save unauthorized end");

    let projection = store
        .poll_projection("$poll:hs")
        .await
        .expect("derive projection")
        .expect("poll exists");
    assert_eq!(projection.status, PollStatus::Open);
    assert!(projection.end.is_none());
}

#[tokio::test]
async fn redacting_the_effective_poll_end_reverts_to_the_next_valid_end() {
    let store = DbStore::connect(&test_db_url("poll-end-redact-rollback"))
        .await
        .expect("connect db");
    store
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            1,
        ))
        .await
        .expect("save poll");
    store
        .save_poll_end(&poll_end("$e1:hs", POLL_CREATOR, 100))
        .await
        .expect("save e1");
    store
        .save_poll_end(&poll_end("$e2:hs", POLL_CREATOR, 200))
        .await
        .expect("save e2");

    assert_eq!(
        store
            .poll_projection("$poll:hs")
            .await
            .unwrap()
            .unwrap()
            .end
            .unwrap()
            .event_id,
        "$e1:hs"
    );

    assert!(
        store
            .redact_poll_end("$e1:hs", Utc::now(), "@mod:hs")
            .await
            .expect("redact e1")
    );
    let projection = store.poll_projection("$poll:hs").await.unwrap().unwrap();
    assert_eq!(projection.status, PollStatus::Ended);
    assert_eq!(projection.end.unwrap().event_id, "$e2:hs");

    // Redacting every end reopens the poll.
    assert!(
        store
            .redact_poll_end("$e2:hs", Utc::now(), "@mod:hs")
            .await
            .expect("redact e2")
    );
    let projection = store.poll_projection("$poll:hs").await.unwrap().unwrap();
    assert_eq!(projection.status, PollStatus::Open);
    assert!(projection.end.is_none());
}

#[tokio::test]
async fn duplicate_poll_end_delivery_is_idempotent() {
    let store = DbStore::connect(&test_db_url("poll-end-duplicate"))
        .await
        .expect("connect db");
    store
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            1,
        ))
        .await
        .expect("save poll");
    let end = poll_end("$e1:hs", POLL_CREATOR, 100);
    store.save_poll_end(&end).await.expect("save end");
    store.save_poll_end(&end).await.expect("re-deliver end");

    let projection = store.poll_projection("$poll:hs").await.unwrap().unwrap();
    assert_eq!(projection.status, PollStatus::Ended);
    assert_eq!(projection.end.unwrap().event_id, "$e1:hs");
    // The event id is unique, so `get_poll_end_by_event` is stable too.
    let stored = store
        .get_poll_end_by_event("$e1:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.sender_mxid, POLL_CREATOR);
    assert_eq!(stored.origin_server_ts, 100);
}

#[tokio::test]
async fn responses_after_the_effective_poll_end_are_ignored() {
    let store = DbStore::connect(&test_db_url("poll-end-vote-window"))
        .await
        .expect("connect db");
    store
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            1,
        ))
        .await
        .expect("save poll");

    let vote = |event_id: &str, sender: &str, ts: i64| PollVote {
        event_id: event_id.to_string(),
        poll_message_id: "$poll:hs".to_string(),
        sender_mxid: sender.to_string(),
        option_index: Some(0),
        origin_server_ts: ts,
    };
    store
        .save_poll_vote_with_selections(&vote("$v-before:hs", "@bob:hs", 100), &["a".into()], None)
        .await
        .expect("vote before close");
    store
        .save_poll_end(&poll_end("$e:hs", POLL_CREATOR, 200))
        .await
        .expect("close poll");
    store
        .save_poll_vote_with_selections(&vote("$v-after:hs", "@carol:hs", 300), &["a".into()], None)
        .await
        .expect("vote after close");

    // Only the vote cast on or before the closing timestamp counts.
    assert_eq!(poll_counts(&store).await, [(0, 1)]);
    let projection = store.poll_projection("$poll:hs").await.unwrap().unwrap();
    assert_eq!(projection.total_votes, 1);
    assert!(projection.vote_of("@carol:hs").is_none());
    assert_eq!(projection.count_for("a"), 1);
}

#[tokio::test]
async fn poll_projection_retains_kind_and_zero_counts() {
    let store = DbStore::connect(&test_db_url("poll-projection-kind"))
        .await
        .expect("connect db");
    store
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.undisclosed",
            2,
        ))
        .await
        .expect("save undisclosed poll");

    let projection = store.poll_projection("$poll:hs").await.unwrap().unwrap();
    assert!(!projection.disclosed, "wire kind must be retained");
    assert_eq!(projection.answer_ids(), vec!["a", "b"]);
    // Zero counts are present for every declared answer.
    assert_eq!(projection.count_for("a"), 0);
    assert_eq!(projection.count_for("b"), 0);

    store
        .save_message(&poll_message(
            "$disclosed:hs",
            "org.matrix.msc3381.poll.disclosed",
            2,
        ))
        .await
        .expect("save disclosed poll");
    assert!(
        store
            .poll_projection("$disclosed:hs")
            .await
            .unwrap()
            .unwrap()
            .disclosed
    );
}

/// The production projection is the reducer: given the same persisted facts,
/// `poll_projection` must agree with `reduce_poll` on every derived field.
/// This fails if independent vote/end reduction logic is reintroduced.
#[tokio::test]
async fn poll_projection_matches_the_canonical_reducer() {
    use cumments_core::poll::{
        EndAuthorization, PollAnswerFact, PollEndFact, PollProjection, PollResponseFact,
        PollStartFact, reduce_poll,
    };

    let store = DbStore::connect(&test_db_url("poll-reducer-parity"))
        .await
        .expect("connect db");
    store
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            2,
        ))
        .await
        .expect("save poll");

    let vote = |event_id: &str, sender: &str, ts: i64| PollVote {
        event_id: event_id.to_string(),
        poll_message_id: "$poll:hs".to_string(),
        sender_mxid: sender.to_string(),
        option_index: None,
        origin_server_ts: ts,
    };
    for (vote, selections) in [
        (
            vote("$v1:hs", "@alice:hs", 1),
            vec!["a".to_string(), "a".to_string(), "b".to_string()],
        ),
        (
            vote("$v2:hs", "@bob:hs", 2),
            vec!["a".to_string(), "unknown".to_string()],
        ),
        (vote("$v3:hs", "@carol:hs", 3), Vec::new()),
        (vote("$v4:hs", "@dave:hs", 4), vec!["b".to_string()]),
    ] {
        store
            .save_poll_vote_with_selections(&vote, &selections, None)
            .await
            .expect("save response");
    }
    store
        .save_poll_end(&poll_end("$e:hs", "@mallory:hs", 5))
        .await
        .expect("save unauthorized end");

    let production: PollProjection = store
        .poll_projection("$poll:hs")
        .await
        .expect("derive projection")
        .expect("poll exists");

    // The same facts, reduced by the canonical reducer directly.
    let start = PollStartFact {
        event_id: "$poll:hs".to_string(),
        sender: POLL_CREATOR.to_string(),
        origin_server_ts: production.created_at,
        question: "best?".to_string(),
        answers: vec![PollAnswerFact::new("a", "A"), PollAnswerFact::new("b", "B")],
        max_selections: 2,
        disclosed: true,
        reply_to: None,
        thread_root: None,
    };
    let responses = vec![
        PollResponseFact::new(
            "$v1:hs",
            "@alice:hs",
            1,
            vec!["a".into(), "a".into(), "b".into()],
        ),
        PollResponseFact::new("$v2:hs", "@bob:hs", 2, vec!["a".into(), "unknown".into()]),
        PollResponseFact::new("$v3:hs", "@carol:hs", 3, vec![]),
        PollResponseFact::new("$v4:hs", "@dave:hs", 4, vec!["b".into()]),
    ];
    let ends = vec![PollEndFact {
        event_id: "$e:hs".to_string(),
        sender: "@mallory:hs".to_string(),
        origin_server_ts: 5,
        authorization: EndAuthorization::from_facts(POLL_CREATOR, "@mallory:hs"),
        redacted: false,
    }];
    let expected = reduce_poll(&start, &responses, &ends).expect("reduce");

    assert_eq!(production.status, expected.status);
    assert_eq!(production.votes, expected.votes);
    assert_eq!(production.tallies, expected.tallies);
    assert_eq!(production.total_votes, expected.total_votes);
    // Duplicates collapsed, unknown spoiled, unvote ignored, tallies per option.
    assert_eq!(production.count_for("a"), 1);
    assert_eq!(production.count_for("b"), 2);
    assert_eq!(production.total_votes, 2);
    assert!(production.vote_of("@bob:hs").unwrap().spoiled);
    assert!(
        production
            .vote_of("@carol:hs")
            .unwrap()
            .selections
            .is_empty()
    );
}

/// Poll facts are retained independently of the poll start, so processing
/// order cannot change the final projection: `response → end → start`
/// converges with `start → response → end`.
#[tokio::test]
async fn poll_facts_recorded_before_the_start_converge() {
    let vote = |event_id: &str, sender: &str, ts: i64| PollVote {
        event_id: event_id.to_string(),
        poll_message_id: "$poll:hs".to_string(),
        sender_mxid: sender.to_string(),
        option_index: None,
        origin_server_ts: ts,
    };
    macro_rules! save_facts {
        ($store:expr) => {{
            $store
                .save_poll_vote_with_selections(
                    &vote("$v1:hs", "@bob:hs", 100),
                    &["a".to_string()],
                    None,
                )
                .await
                .expect("save response");
            $store
                .save_poll_end(&poll_end("$e:hs", POLL_CREATOR, 300))
                .await
                .expect("save end");
        }};
    }

    // Poll-first ordering.
    let start_first = DbStore::connect(&test_db_url("poll-order-start-first"))
        .await
        .expect("connect db");
    start_first
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            1,
        ))
        .await
        .expect("save poll");
    save_facts!(start_first);

    // Facts-first ordering: the poll start has not been projected yet when the
    // response and end arrive, so they must be retained until it appears.
    let facts_first = DbStore::connect(&test_db_url("poll-order-facts-first"))
        .await
        .expect("connect db");
    save_facts!(facts_first);
    assert!(
        facts_first
            .poll_projection("$poll:hs")
            .await
            .expect("derive without start")
            .is_none(),
        "no projection exists before the poll start is projected"
    );
    facts_first
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            1,
        ))
        .await
        .expect("save poll after facts");

    let a = start_first
        .poll_projection("$poll:hs")
        .await
        .unwrap()
        .unwrap();
    let b = facts_first
        .poll_projection("$poll:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.status, b.status);
    assert_eq!(a.votes, b.votes);
    assert_eq!(a.tallies, b.tallies);
    assert_eq!(a.total_votes, b.total_votes);
    assert_eq!(a.end, b.end);
    assert_eq!(b.status, PollStatus::Ended);
    assert_eq!(b.count_for("a"), 1);
}

/// End authorization is derived from the facts (the poll creator), never from
/// a room-power snapshot taken at local processing time. A non-creator end is
/// retained as a fact but stays ineffective: the Cumments site-moderator to
/// Matrix redact-power mapping is an explicit unresolved boundary for a later
/// authorization layer.
#[tokio::test]
async fn poll_end_authorization_is_derived_from_facts() {
    let store = DbStore::connect(&test_db_url("poll-end-facts-auth"))
        .await
        .expect("connect db");
    store
        .save_message(&poll_message(
            "$poll:hs",
            "org.matrix.msc3381.poll.disclosed",
            1,
        ))
        .await
        .expect("save poll");

    // A redact-power-style end from another user is a fact, but not effective.
    store
        .save_poll_end(&poll_end("$other:hs", "@moderator:hs", 100))
        .await
        .expect("save non-creator end");
    let projection = store.poll_projection("$poll:hs").await.unwrap().unwrap();
    assert_eq!(projection.status, PollStatus::Open);
    assert!(projection.end.is_none());
    // The fact is preserved for the later authorization layer.
    assert!(
        store
            .get_poll_end_by_event("$other:hs")
            .await
            .unwrap()
            .is_some()
    );

    // A creator end is effective.
    store
        .save_poll_end(&poll_end("$creator:hs", POLL_CREATOR, 200))
        .await
        .expect("save creator end");
    let projection = store.poll_projection("$poll:hs").await.unwrap().unwrap();
    assert_eq!(projection.status, PollStatus::Ended);
    assert_eq!(projection.end.unwrap().event_id, "$creator:hs");
}

use chrono::Utc;
use cumments_core::models::{AuthorType, Comment, CommentAuthor, PostSlug, SiteId};
use cumments_core::ports::{CommentStore, VirtualUserStore};
use cumments_store::DbStore;

/// Unique SQLite file per test to avoid shared in-memory state.
fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn save_comment_records_original_sender() {
    let store = DbStore::connect(&test_db_url("comment-sender"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");

    let comment = Comment {
        event_id: "$event:hs".to_string(),
        site_id: site.as_str().to_string(),
        post_slug: slug.as_str().to_string(),
        author: CommentAuthor {
            kind: AuthorType::Guest,
            display_name: Some("Alice".to_string()),
            public_key: Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string()),
            mxid: None,
        },
        content: "hello".to_string(),
        timestamp: Utc::now(),
        reply_to: Some("$parent:hs".to_string()),
        intent_id: Some(42),
        room_id: "!room:hs".to_string(),
        sender_mxid: String::new(),
    };

    store
        .save_comment(
            &comment,
            "!room:hs",
            "@_cumments_my-blog_a1b2c3d4e5f60718:hs",
            &site,
            &slug,
        )
        .await
        .expect("save comment");

    let stored = store
        .get_comment("$event:hs")
        .await
        .expect("get comment")
        .expect("comment exists");
    assert_eq!(stored.room_id, "!room:hs");
    assert_eq!(stored.sender_mxid, "@_cumments_my-blog_a1b2c3d4e5f60718:hs");
    assert_eq!(stored.author.kind, AuthorType::Guest);
    assert_eq!(
        stored.author.public_key.as_deref(),
        Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc")
    );
    assert_eq!(stored.reply_to.as_deref(), Some("$parent:hs"));
    assert_eq!(stored.intent_id, Some(42));
}

#[tokio::test]
async fn update_comment_preserves_reply_to() {
    let store = DbStore::connect(&test_db_url("comment-reply-edit"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");

    let comment = Comment {
        event_id: "$event:hs".to_string(),
        site_id: site.as_str().to_string(),
        post_slug: slug.as_str().to_string(),
        author: CommentAuthor {
            kind: AuthorType::Guest,
            display_name: Some("Alice".to_string()),
            public_key: Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string()),
            mxid: None,
        },
        content: "original".to_string(),
        timestamp: Utc::now(),
        reply_to: Some("$parent:hs".to_string()),
        intent_id: None,
        room_id: "!room:hs".to_string(),
        sender_mxid: String::new(),
    };

    store
        .save_comment(
            &comment,
            "!room:hs",
            "@_cumments_my-blog_a1b2c3d4e5f60718:hs",
            &site,
            &slug,
        )
        .await
        .expect("save comment");

    assert!(
        store
            .update_comment_content("$event:hs", "!room:hs", "edited", 200, "$edit:hs")
            .await
            .expect("update comment")
    );

    let stored = store
        .get_comment("$event:hs")
        .await
        .expect("get comment")
        .expect("comment exists");
    assert_eq!(stored.content, "edited");
    assert_eq!(stored.reply_to.as_deref(), Some("$parent:hs"));
}

#[tokio::test]
async fn comments_with_equal_timestamps_sort_by_event_id() {
    let store = DbStore::connect(&test_db_url("comment-order"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");
    let ts = Utc::now();

    for (event_id, content) in [("$b:hs", "second"), ("$a:hs", "first")] {
        let comment = Comment {
            event_id: event_id.to_string(),
            site_id: site.as_str().to_string(),
            post_slug: slug.as_str().to_string(),
            author: CommentAuthor {
                kind: AuthorType::Guest,
                display_name: Some("Alice".to_string()),
                public_key: Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string()),
                mxid: None,
            },
            content: content.to_string(),
            timestamp: ts,
            reply_to: None,
            intent_id: None,
            room_id: "!room:hs".to_string(),
            sender_mxid: String::new(),
        };
        store
            .save_comment(
                &comment,
                "!room:hs",
                "@_cumments_my-blog_a1b2c3d4e5f60718:hs",
                &site,
                &slug,
            )
            .await
            .expect("save comment");
    }

    let page = store
        .get_comments(&site, &slug, 10, 0)
        .await
        .expect("query comments");
    let comments = page.items;
    assert_eq!(page.total, 2);
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].event_id, "$a:hs");
    assert_eq!(comments[1].event_id, "$b:hs");
}

#[tokio::test]
async fn matrix_native_comment_roundtrip() {
    let store = DbStore::connect(&test_db_url("comment-matrix"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");

    let comment = Comment {
        event_id: "$matrix:hs".to_string(),
        site_id: site.as_str().to_string(),
        post_slug: slug.as_str().to_string(),
        author: CommentAuthor {
            kind: AuthorType::Matrix,
            display_name: None,
            public_key: None,
            mxid: Some("@alice:hs".to_string()),
        },
        content: "from matrix".to_string(),
        timestamp: Utc::now(),
        reply_to: None,
        intent_id: None,
        room_id: "!room:hs".to_string(),
        sender_mxid: "@alice:hs".to_string(),
    };

    store
        .save_comment(&comment, "!room:hs", "@alice:hs", &site, &slug)
        .await
        .expect("save comment");

    let stored = store
        .get_comment("$matrix:hs")
        .await
        .expect("get comment")
        .expect("comment exists");
    assert_eq!(stored.author.kind, AuthorType::Matrix);
    assert!(stored.author.public_key.is_none());
    assert_eq!(stored.author.mxid.as_deref(), Some("@alice:hs"));
    assert_eq!(stored.sender_mxid, "@alice:hs");
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

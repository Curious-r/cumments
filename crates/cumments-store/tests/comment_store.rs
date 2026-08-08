use chrono::Utc;
use cumments_core::models::{Comment, PostSlug, SiteId};
use cumments_core::ports::CommentStore;
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
        author_nickname: Some("Alice".to_string()),
        author_public_key: Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string()),
        content: "hello".to_string(),
        timestamp: Utc::now(),
        room_id: "!room:hs".to_string(),
        author_mxid: String::new(),
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
    assert_eq!(stored.author_mxid, "@_cumments_my-blog_a1b2c3d4e5f60718:hs");
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
            author_nickname: Some("Alice".to_string()),
            author_public_key: Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string()),
            content: content.to_string(),
            timestamp: ts,
            room_id: "!room:hs".to_string(),
            author_mxid: String::new(),
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

    let (comments, _) = store
        .get_comments(&site, &slug, 10, 0)
        .await
        .expect("query comments");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].event_id, "$a:hs");
    assert_eq!(comments[1].event_id, "$b:hs");
}

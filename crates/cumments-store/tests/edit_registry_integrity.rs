use chrono::Utc;
use cumments_core::models::{AuthorType, Comment, CommentAuthor, PostSlug, SiteId};
use cumments_core::ports::{CommentStore, RegistryStore};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-edit-registry-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn save_comment(store: &DbStore, event_id: &str, room_id: &str, content: &str) {
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");
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
        timestamp: Utc::now(),
        edited_at: None,
        reply_to: None,
        intent_id: None,
        room_id: room_id.to_string(),
        sender_mxid: String::new(),
    };
    store
        .save_comment(
            &comment,
            room_id,
            "@_cumments_my-blog_a1b2c3d4e5f60718:hs",
            &site,
            &slug,
        )
        .await
        .expect("save comment");
}

#[tokio::test]
async fn edit_from_different_room_is_rejected() {
    let store = DbStore::connect(&test_db_url("room-bind"))
        .await
        .expect("connect db");
    save_comment(&store, "$event:hs", "!room-a:hs", "original").await;

    let applied = store
        .update_comment_content("$event:hs", "!room-b:hs", "edited", 200, "$edit:hs")
        .await
        .expect("update comment");
    assert!(!applied, "edit from another room must be rejected");

    let stored = store
        .get_comment("$event:hs")
        .await
        .expect("get comment")
        .expect("comment exists");
    assert_eq!(stored.content, "original");
}

#[tokio::test]
async fn stale_edit_is_rejected_and_event_id_breaks_ties() {
    let store = DbStore::connect(&test_db_url("edit-order"))
        .await
        .expect("connect db");
    save_comment(&store, "$event:hs", "!room-a:hs", "original").await;

    let applied = store
        .update_comment_content("$event:hs", "!room-a:hs", "two", 200, "$e2:hs")
        .await
        .expect("update comment");
    assert!(applied);

    let applied = store
        .update_comment_content("$event:hs", "!room-a:hs", "one", 100, "$e1:hs")
        .await
        .expect("update comment");
    assert!(!applied, "older edit must be ignored");

    let applied = store
        .update_comment_content("$event:hs", "!room-a:hs", "tie-loser", 200, "$e0:hs")
        .await
        .expect("update comment");
    assert!(
        !applied,
        "equal timestamp with smaller event id must be ignored"
    );

    let applied = store
        .update_comment_content("$event:hs", "!room-a:hs", "three", 300, "$e3:hs")
        .await
        .expect("update comment");
    assert!(applied);

    let stored = store
        .get_comment("$event:hs")
        .await
        .expect("get comment")
        .expect("comment exists");
    assert_eq!(stored.content, "three");
}

#[tokio::test]
async fn register_room_deactivates_previous_active_room() {
    let store = DbStore::connect(&test_db_url("registry-unique"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");

    store
        .register_room("!room-a:hs", &site, &slug)
        .await
        .expect("register first room");
    store
        .register_room("!room-b:hs", &site, &slug)
        .await
        .expect("register second room");

    let active = store
        .get_registered_room(&site, &slug)
        .await
        .expect("query registry");
    assert_eq!(active.as_deref(), Some("!room-b:hs"));
    assert_eq!(
        store.is_room_active("!room-a:hs").await.expect("room a"),
        Some(false)
    );

    // Re-registering the old room moves activity back to it.
    store
        .register_room("!room-a:hs", &site, &slug)
        .await
        .expect("reactivate first room");
    let active = store
        .get_registered_room(&site, &slug)
        .await
        .expect("query registry");
    assert_eq!(active.as_deref(), Some("!room-a:hs"));
    assert_eq!(
        store.is_room_active("!room-b:hs").await.expect("room b"),
        Some(false)
    );
}

#[tokio::test]
async fn blocked_rooms_are_listed_and_cleared_on_register() {
    let store = DbStore::connect(&test_db_url("registry-blocked"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");

    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");
    store
        .mark_room_blocked(
            "!room:hs",
            "Refusing to adopt room: AS sender cannot write state",
        )
        .await
        .expect("mark blocked");

    let blocked = store.get_blocked_rooms().await.expect("list blocked");
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].room_id, "!room:hs");
    assert_eq!(blocked[0].site_id, "my-blog");
    assert!(blocked[0].reason.contains("Refusing to adopt"));
    assert_eq!(
        store.is_room_active("!room:hs").await.expect("room a"),
        Some(false)
    );

    // A later successful registration clears the blocked state.
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register again");
    assert!(
        store
            .get_blocked_rooms()
            .await
            .expect("list blocked")
            .is_empty()
    );
    assert_eq!(
        store.is_room_active("!room:hs").await.expect("room a"),
        Some(true)
    );
}

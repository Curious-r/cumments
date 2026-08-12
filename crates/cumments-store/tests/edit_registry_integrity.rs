use chrono::Utc;
use cumments_core::models::{AuthorType, Comment, CommentAuthor, PostSlug, RoomStatus, SiteId};
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
        store.get_room_status("!room-a:hs").await.expect("room a"),
        Some(RoomStatus::Superseded)
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
        store.get_room_status("!room-b:hs").await.expect("room b"),
        Some(RoomStatus::Superseded)
    );
}

#[tokio::test]
async fn quarantined_rooms_are_listed_and_cleared_on_register() {
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
        .quarantine_room(
            "!room:hs",
            "Refusing to adopt room: AS sender cannot write state",
            1,
            None,
        )
        .await
        .expect("quarantine room");

    let quarantined = store
        .get_quarantined_rooms()
        .await
        .expect("list quarantined");
    assert_eq!(quarantined.len(), 1);
    assert_eq!(quarantined[0].room_id, "!room:hs");
    assert_eq!(quarantined[0].site_id, "my-blog");
    assert!(
        quarantined[0]
            .quarantine_reason
            .contains("Refusing to adopt")
    );
    assert_eq!(
        store.get_room_status("!room:hs").await.expect("room a"),
        Some(RoomStatus::Quarantined)
    );

    // A later successful registration clears the quarantine.
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register again");
    assert!(
        store
            .get_quarantined_rooms()
            .await
            .expect("list quarantined")
            .is_empty()
    );
    assert_eq!(
        store.get_room_status("!room:hs").await.expect("room a"),
        Some(RoomStatus::Active)
    );
}

#[tokio::test]
async fn quarantine_tracks_failures_and_preserves_first_time() {
    let store = DbStore::connect(&test_db_url("registry-quarantine-count"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");

    let first_quarantine = Utc::now();
    let first_attempt = first_quarantine + chrono::Duration::hours(1);
    store
        .quarantine_room("!room:hs", "first refusal", 1, Some(first_attempt))
        .await
        .expect("first quarantine");
    let second_attempt = Utc::now() + chrono::Duration::hours(6);
    store
        .quarantine_room("!room:hs", "second refusal", 2, Some(second_attempt))
        .await
        .expect("second quarantine");

    let quarantined = store
        .get_quarantined_rooms()
        .await
        .expect("list quarantined");
    assert_eq!(quarantined.len(), 1);
    assert_eq!(quarantined[0].adoption_failures, 2);
    assert_eq!(quarantined[0].quarantine_reason, "second refusal");
    assert!(
        quarantined[0].quarantined_at >= first_quarantine
            && quarantined[0].quarantined_at <= Utc::now(),
        "first quarantine time must be preserved"
    );
    assert_eq!(
        quarantined[0].next_attempt_at.map(|t| t.timestamp()),
        Some(second_attempt.timestamp())
    );
}

#[tokio::test]
async fn reinstate_supersedes_other_active_room() {
    let store = DbStore::connect(&test_db_url("registry-reinstate"))
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
    store
        .quarantine_room("!room-b:hs", "refused", 1, None)
        .await
        .expect("quarantine current room");

    assert!(store.reinstate_room("!room-b:hs").await.expect("reinstate"));
    assert_eq!(
        store.get_room_status("!room-a:hs").await.expect("room a"),
        Some(RoomStatus::Superseded),
        "reinstating must supersede the other active room"
    );
    assert_eq!(
        store.get_room_status("!room-b:hs").await.expect("room b"),
        Some(RoomStatus::Active)
    );
    assert_eq!(
        store
            .get_registered_room(&site, &slug)
            .await
            .expect("query registry")
            .as_deref(),
        Some("!room-b:hs")
    );
    assert!(
        !store
            .reinstate_room("!unknown:hs")
            .await
            .expect("unknown room"),
        "unknown room must return false"
    );
}

#[tokio::test]
async fn retire_room_marks_superseded_and_clears_schedule() {
    let store = DbStore::connect(&test_db_url("registry-retire"))
        .await
        .expect("connect db");
    let site = SiteId::from("my-blog");
    let slug = PostSlug::from("hello");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");
    store
        .quarantine_room(
            "!room:hs",
            "refused",
            1,
            Some(Utc::now() + chrono::Duration::hours(1)),
        )
        .await
        .expect("quarantine room");

    store.retire_room("!room:hs").await.expect("retire room");
    assert_eq!(
        store.get_room_status("!room:hs").await.expect("room"),
        Some(RoomStatus::Superseded)
    );
    assert!(
        store
            .get_quarantined_rooms()
            .await
            .expect("list quarantined")
            .is_empty(),
        "retired rooms must not appear as quarantined"
    );
}

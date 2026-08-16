use cumments_core::{
    models::{
        AuthorKind, AuthorSnapshot, Content, Message, MessageStatus, PostSlug, RoomStatus, SiteId,
        TextContent, TextStyle,
    },
    ports::{MessageStore, RegistryStore, SiteAuthStore},
};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-registry-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn register_if_absent_never_resurrects_quarantined_or_superseded_rooms() {
    let store = DbStore::connect(&test_db_url("backfill-status"))
        .await
        .expect("connect db");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let post_slug = PostSlug::new("hello".to_string()).expect("post slug");

    store
        .register_room("!room:hs", &site_id, &post_slug)
        .await
        .expect("register room");
    store
        .quarantine_room("!room:hs", "adoption failed", 1, None)
        .await
        .expect("quarantine room");

    store
        .register_room_if_absent("!room:hs", &site_id, &post_slug)
        .await
        .expect("register if absent");
    assert_eq!(
        store.get_room_status("!room:hs").await.unwrap(),
        Some(RoomStatus::Quarantined),
        "backfill must not resurrect a quarantined room"
    );

    store
        .register_room_if_absent("!new-room:hs", &site_id, &post_slug)
        .await
        .expect("register new room");
    assert_eq!(
        store.get_room_status("!new-room:hs").await.unwrap(),
        Some(RoomStatus::Active),
        "a genuinely new discovered room registers as active"
    );

    store
        .retire_room("!new-room:hs")
        .await
        .expect("retire room");
    store
        .register_room_if_absent("!new-room:hs", &site_id, &post_slug)
        .await
        .expect("register if absent after retire");
    assert_eq!(
        store.get_room_status("!new-room:hs").await.unwrap(),
        Some(RoomStatus::Superseded),
        "backfill must not resurrect a superseded room"
    );

    // Decommission enumeration must see every lifecycle state.
    let mut all = store
        .list_rooms_for_site(&site_id)
        .await
        .expect("list all rooms for site");
    all.sort();
    assert_eq!(all, vec!["!new-room:hs", "!room:hs"]);

    let mut superseded = store
        .list_superseded_rooms()
        .await
        .expect("list superseded rooms");
    superseded.sort();
    assert_eq!(superseded, vec!["!new-room:hs"]);
}

#[tokio::test]
async fn mark_room_retired_stops_active_lookup_and_lists_retired() {
    let store = DbStore::connect(&test_db_url("retired"))
        .await
        .expect("connect db");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let post_slug = PostSlug::new("hello".to_string()).expect("post slug");
    store
        .register_room("!room:hs", &site_id, &post_slug)
        .await
        .expect("register room");

    assert!(
        store
            .mark_room_retired("!room:hs")
            .await
            .expect("mark retired")
    );
    assert!(
        !store
            .mark_room_retired("!room:hs")
            .await
            .expect("second mark is a no-op")
    );
    assert!(
        !store
            .mark_room_retired("!missing:hs")
            .await
            .expect("missing room is false")
    );

    assert_eq!(
        store
            .get_registered_room(&site_id, &post_slug)
            .await
            .expect("active lookup"),
        None,
        "retired rooms must not resolve as active write targets"
    );
    assert_eq!(
        store
            .get_room_status("!room:hs")
            .await
            .expect("room status"),
        Some(RoomStatus::Retired)
    );
    assert_eq!(
        store
            .list_retired_rooms()
            .await
            .expect("list retired rooms"),
        vec!["!room:hs".to_string()]
    );
}

#[tokio::test]
async fn delete_room_local_clears_the_room_and_keeps_avatar_media() {
    let store = DbStore::connect(&test_db_url("delete-room"))
        .await
        .expect("connect db");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let post_slug = PostSlug::new("hello".to_string()).expect("post slug");
    store
        .register_room("!room:hs", &site_id, &post_slug)
        .await
        .expect("register room");

    let message = Message {
        event_id: "$m:hs".to_string(),
        site_id: "my-blog".to_string(),
        post_slug: "hello".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Guest,
            display_name: None,
            avatar_url: None,
            public_key: None,
            mxid: None,
        },
        content: Content::Text(TextContent {
            body: "hi".to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        timestamp: chrono::Utc::now(),
        edited_at: None,
        reply_to: None,
        thread_root: None,
        submission_id: None,
        status: MessageStatus::Active,
        redacted_at: None,
        redacted_by: None,
        reactions: Vec::new(),
        room_id: "!room:hs".to_string(),
        sender_mxid: "@_cumments_my-blog_abc:hs".to_string(),
        raw_content: serde_json::json!({}),
    };
    store.save_message(&message).await.expect("save message");
    store
        .record_media_upload("mxc://hs/cat", "key", "my-blog", Some("hello"))
        .await
        .expect("record comment media");
    store
        .record_media_upload("mxc://hs/avatar", "key", "my-blog", None)
        .await
        .expect("record avatar media");

    store
        .delete_room_local("!room:hs")
        .await
        .expect("delete room local");

    assert!(
        store
            .get_message("$m:hs")
            .await
            .expect("message query")
            .is_none(),
        "messages must be cleared with the room"
    );
    assert!(
        store
            .get_registered_room_identity("!room:hs")
            .await
            .expect("registry query")
            .is_none(),
        "registry row must be removed"
    );
    assert!(
        !store
            .media_upload_owned_by("mxc://hs/cat", "key", "my-blog", "hello")
            .await
            .expect("media ownership query"),
        "post media rows must be cleared"
    );
    let remaining = store
        .list_media_urls_for_site("my-blog")
        .await
        .expect("remaining media");
    assert_eq!(
        remaining,
        vec!["mxc://hs/avatar".to_string()],
        "avatar media (post_slug NULL) is site-scoped and must survive"
    );
}

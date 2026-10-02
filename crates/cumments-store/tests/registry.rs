use cumments_core::{
    models::{
        AuthorKind, AuthorSnapshot, Content, Message, MessageStatus, PageSlug, RoomStatus, SiteId,
        TextContent, TextStyle,
    },
    ports::{MessageStore, RegistryStore, SiteAuthStore},
};
use cumments_store::DbStore;
use sea_orm::{ConnectionTrait, Statement};

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
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");

    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    store
        .quarantine_room("!room:hs", "adoption failed", 1, None)
        .await
        .expect("quarantine room");

    store
        .register_room_if_absent("!room:hs", &site_id, &page_slug)
        .await
        .expect("register if absent");
    assert_eq!(
        store.get_room_status("!room:hs").await.unwrap(),
        Some(RoomStatus::Quarantined),
        "backfill must not resurrect a quarantined room"
    );

    store
        .register_room_if_absent("!new-room:hs", &site_id, &page_slug)
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
        .register_room_if_absent("!new-room:hs", &site_id, &page_slug)
        .await
        .expect("register if absent after retire");
    assert_eq!(
        store.get_room_status("!new-room:hs").await.unwrap(),
        Some(RoomStatus::Superseded),
        "backfill must not resurrect a superseded room"
    );

    // Retirement enumeration must see every lifecycle state.
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
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_room("!room:hs", &site_id, &page_slug)
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
            .get_registered_room(&site_id, &page_slug)
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
    let db_url = test_db_url("delete-room");
    let store = DbStore::connect(&db_url).await.expect("connect db");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");

    let message = Message {
        event_id: "$m:hs".to_string(),
        site_id: "my-blog".to_string(),
        page_slug: "hello".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Visitor,
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
        matrix_event_type: "m.room.message".to_string(),
        timestamp: chrono::Utc::now(),
        edited_at: None,
        reply_to: None,
        thread_root: None,
        submission_id: None,
        status: MessageStatus::Active,
        redacted_at: None,
        redacted_by: None,
        reactions: Vec::new(),

        thread_summary: None,
        room_id: "!room:hs".to_string(),
        sender_mxid: "@_cumments_my-blog_abc:hs".to_string(),
        raw_content: serde_json::json!({}),
    };
    store.save_message(&message).await.expect("save message");
    for (mxc, page) in [("mxc://hs/cat", Some("hello")), ("mxc://hs/avatar", None)] {
        store
            .save_media_upload_idempotent(
                mxc,
                "key",
                "my-blog",
                page,
                &cumments_core::media_upload::MediaUploadIdempotencyInput {
                    key: format!("upload-{mxc}"),
                    request_fingerprint: mxc.to_string(),
                },
            )
            .await
            .expect("record upload");
    }

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
            .expect("media upload query"),
        "page-scoped upload rows must be cleared with the room"
    );

    // The site-scoped avatar upload (page_slug NULL) must survive.
    let db = sea_orm::Database::connect(&db_url)
        .await
        .expect("connect raw db");
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT mxc_url FROM media_uploads WHERE site_id = 'my-blog' ORDER BY mxc_url",
        ))
        .await
        .expect("query remaining uploads");
    let remaining: Vec<String> = rows
        .iter()
        .map(|row| row.try_get("", "mxc_url").expect("mxc_url"))
        .collect();
    assert_eq!(
        remaining,
        vec!["mxc://hs/avatar".to_string()],
        "avatar upload (page_slug NULL) is site-scoped and must survive"
    );
}

#[tokio::test]
async fn register_room_refuses_to_reactivate_a_retired_room() {
    let store = DbStore::connect(&test_db_url("retired-register"))
        .await
        .expect("connect db");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");

    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    assert!(
        store
            .mark_room_retired("!room:hs")
            .await
            .expect("mark retired")
    );

    let result = store.register_room("!room:hs", &site_id, &page_slug).await;
    assert!(
        result.is_err(),
        "a retired instance must never be reactivated by registration"
    );
    assert_eq!(
        store.get_room_status("!room:hs").await.expect("status"),
        Some(RoomStatus::Retired),
        "the refused registration must leave the row retired"
    );
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
            .await
            .expect("active lookup"),
        None,
        "a retired room must not become the active write target"
    );
}

#[tokio::test]
async fn reinstate_refuses_retired_rooms_but_still_supports_quarantined() {
    let store = DbStore::connect(&test_db_url("retired-reinstate"))
        .await
        .expect("connect db");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");

    // Quarantined rooms remain reinstatable.
    store
        .register_room("!quarantined:hs", &site_id, &page_slug)
        .await
        .expect("register quarantined room");
    store
        .quarantine_room("!quarantined:hs", "refused", 1, None)
        .await
        .expect("quarantine room");
    assert!(
        store
            .reinstate_room("!quarantined:hs")
            .await
            .expect("reinstate quarantined room"),
        "quarantine reinstatement must keep working"
    );
    assert_eq!(
        store
            .get_room_status("!quarantined:hs")
            .await
            .expect("status"),
        Some(RoomStatus::Active)
    );

    // Retired rooms are terminal.
    store
        .register_room("!retired:hs", &site_id, &page_slug)
        .await
        .expect("register retired room");
    assert!(
        store
            .mark_room_retired("!retired:hs")
            .await
            .expect("mark retired")
    );
    assert!(
        !store
            .reinstate_room("!retired:hs")
            .await
            .expect("reinstate retired room"),
        "a retired instance must not be reinstatable"
    );
    assert_eq!(
        store.get_room_status("!retired:hs").await.expect("status"),
        Some(RoomStatus::Retired)
    );
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
            .await
            .expect("active lookup"),
        None,
        "reinstating a retired room must not revive the page's write target"
    );
}

#[tokio::test]
async fn quarantine_does_not_mutate_a_retired_room() {
    let store = DbStore::connect(&test_db_url("retired-quarantine"))
        .await
        .expect("connect db");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");

    // A retired room is terminal: a late adoption failure must not move it out
    // of `Retired`.
    store
        .register_room("!retired:hs", &site_id, &page_slug)
        .await
        .expect("register retired room");
    assert!(
        store
            .mark_room_retired("!retired:hs")
            .await
            .expect("mark retired")
    );
    store
        .quarantine_room("!retired:hs", "late adoption failure", 1, None)
        .await
        .expect("quarantine retired room");
    assert_eq!(
        store.get_room_status("!retired:hs").await.expect("status"),
        Some(RoomStatus::Retired),
        "quarantining must not revive a retired room"
    );
    assert!(
        store
            .get_quarantined_rooms()
            .await
            .expect("list quarantined")
            .is_empty(),
        "a retired room must not appear as quarantined"
    );

    // Non-retired rooms keep their existing quarantine behaviour.
    store
        .register_room("!active:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    store
        .quarantine_room("!active:hs", "refused", 1, None)
        .await
        .expect("quarantine room");
    assert_eq!(
        store.get_room_status("!active:hs").await.expect("status"),
        Some(RoomStatus::Quarantined),
        "quarantining an active room must keep working"
    );
}

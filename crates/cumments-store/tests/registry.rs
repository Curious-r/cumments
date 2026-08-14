use cumments_core::{
    models::{PostSlug, RoomStatus, SiteId},
    ports::RegistryStore,
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
}

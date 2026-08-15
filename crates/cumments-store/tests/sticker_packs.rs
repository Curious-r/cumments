use cumments_core::ports::StickerPackStore;
use cumments_core::sticker_packs::{
    StickerImage, StickerPack, StickerPackContent, StickerPackProjection,
};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-stickers-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn projection(site: &str, state_key: &str, event_id: &str, ts: i64) -> StickerPackProjection {
    StickerPackProjection {
        pack: StickerPack {
            room_id: "!space:hs".to_string(),
            site_id: site.to_string(),
            state_key: state_key.to_string(),
            content: StickerPackContent {
                display_name: Some(state_key.to_string()),
                usage: vec!["sticker".to_string()],
                images: vec![StickerImage {
                    shortcode: "cat".to_string(),
                    url: "mxc://hs/1".to_string(),
                    body: None,
                    info: None,
                }],
                ..Default::default()
            },
        },
        event_id: event_id.to_string(),
        sender: "@owner:hs".to_string(),
        origin_server_ts: ts,
    }
}

#[tokio::test]
async fn packs_upsert_by_site_and_state_key_latest_wins() {
    let store = DbStore::connect(&test_db_url("upsert"))
        .await
        .expect("connect db");

    store
        .save_site_pack(&projection("site-a", "default", "$v1", 100))
        .await
        .expect("save v1");
    store
        .save_site_pack(&projection("site-a", "extra", "$v2", 200))
        .await
        .expect("save v2");
    store
        .save_site_pack(&projection("site-b", "default", "$v3", 300))
        .await
        .expect("save site-b");

    let site_a = store.list_site_packs("site-a").await.expect("list site-a");
    assert_eq!(
        site_a
            .iter()
            .map(|p| (p.pack.state_key.as_str(), p.event_id.as_str()))
            .collect::<Vec<_>>(),
        vec![("default", "$v1"), ("extra", "$v2")]
    );

    // A later state event for the same pack wins.
    store
        .save_site_pack(&projection("site-a", "default", "$v4", 400))
        .await
        .expect("save v4");
    let current = store
        .get_site_pack("site-a", "default")
        .await
        .expect("get")
        .expect("pack exists");
    assert_eq!(current.event_id, "$v4");

    let site_b = store.list_site_packs("site-b").await.expect("list site-b");
    assert_eq!(site_b.len(), 1);
    assert_eq!(site_b[0].pack.state_key, "default");
}

#[tokio::test]
async fn delete_and_event_id_lookup() {
    let store = DbStore::connect(&test_db_url("delete"))
        .await
        .expect("connect db");
    store
        .save_site_pack(&projection("site-a", "default", "$v1", 100))
        .await
        .expect("save");

    let found = store
        .find_pack_by_event_id("$v1")
        .await
        .expect("lookup")
        .expect("found");
    assert_eq!(found, ("site-a".to_string(), "default".to_string()));

    store
        .delete_site_pack("site-a", "default")
        .await
        .expect("delete");
    assert!(
        store
            .get_site_pack("site-a", "default")
            .await
            .expect("get after delete")
            .is_none()
    );
    assert!(
        store
            .find_pack_by_event_id("$v1")
            .await
            .expect("lookup after delete")
            .is_none()
    );
}

#[tokio::test]
async fn pack_json_round_trips_normalized_content() {
    let store = DbStore::connect(&test_db_url("roundtrip"))
        .await
        .expect("connect db");
    let pack = projection("site-a", "default", "$v1", 100);
    store.save_site_pack(&pack).await.expect("save");

    let loaded = store
        .get_site_pack("site-a", "default")
        .await
        .expect("get")
        .expect("pack");
    assert_eq!(loaded.pack, pack.pack);
    assert_eq!(loaded.sender, "@owner:hs");
    assert_eq!(loaded.origin_server_ts, 100);
}

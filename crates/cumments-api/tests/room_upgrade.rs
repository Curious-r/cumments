//! Integration tests for the shared room-upgrade management use case.
//!
//! The use case lives in `cumments-core` and is shared by the CLI, API and
//! bot; these tests drive it with a real registry/site store and the
//! in-memory Matrix driver so every convergence write is asserted.

use cumments_core::management::{ManagementError, upgrade_comment_room};
use cumments_core::models::{PostSlug, RoomStatus, Site, SiteId};
use cumments_core::ports::{RegistryStore, SiteStore};
use cumments_core::site_service::SiteService;
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;
use serde_json::json;
use std::sync::Arc;

fn test_db_url(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "cumments-room-upgrade-test-{name}-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn test_fixture(name: &str) -> (DbStore, TestDriver, SiteService) {
    let store = DbStore::connect(&test_db_url(name))
        .await
        .expect("connect test database");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let post_slug = PostSlug::new("hello".to_string()).expect("post slug");
    store
        .register_room("!old:hs", &site_id, &post_slug)
        .await
        .expect("register old room");
    // Pre-seed the site -> Space mapping so SiteService does not need to
    // create a Space through the driver.
    store
        .save_site(&Site {
            id: site_id.as_str().to_string(),
            matrix_space_id: "!space:hs".to_string(),
            display_name: Some("my-blog".to_string()),
            created_at: chrono::Utc::now(),
        })
        .await
        .expect("save site");

    let driver = TestDriver::new();
    driver.power_levels.lock().await.insert(
        "!space:hs".to_string(),
        json!({
            "users": { "@owner:hs": 100, "@co:hs": 75 },
            "events": {
                "m.room.power_levels": 100,
                "m.room.tombstone": 100,
            },
            "state_default": 50,
        }),
    );

    let site_service = SiteService::new(Arc::new(store.clone()));
    (store, driver, site_service)
}

#[tokio::test]
async fn upgrade_comment_room_converges_the_replacement() {
    let (store, driver, site_service) = test_fixture("converge").await;

    let replacement = upgrade_comment_room(&driver, &store, &site_service, "!old:hs", "12")
        .await
        .expect("upgrade must succeed");
    assert_eq!(replacement, "!upgraded-1:hs");

    // The native upgrade was requested with the explicit target version.
    assert_eq!(
        *driver.upgrades.lock().await,
        vec![("!old:hs".to_string(), "12".to_string())]
    );

    // Convergence: adoption repairs metadata, the Space child is re-linked,
    // the old child's via is cleared, and site roles are re-invited.
    assert!(driver.adoptions.lock().await.contains(&replacement));
    assert_eq!(
        *driver.space_links.lock().await,
        vec![("!space:hs".to_string(), replacement.clone())]
    );
    assert!(driver.state_writes.lock().await.contains(&(
        "!space:hs".to_string(),
        "m.space.child".to_string(),
        "!old:hs".to_string()
    )));
    let invites = driver.invites.lock().await.clone();
    assert!(invites.contains(&(replacement.clone(), "@owner:hs".to_string())));
    assert!(invites.contains(&(replacement.clone(), "@co:hs".to_string())));
    assert!(!invites.iter().any(|(_, user)| user == "@_cumments_bot:hs"));

    let metadata = driver
        .room_metadata
        .lock()
        .await
        .get(&replacement)
        .cloned()
        .expect("replacement metadata");
    assert_eq!(metadata["site_id"], "my-blog");
    assert_eq!(metadata["post_slug"], "hello");

    // Registry: the replacement is active and the old room is superseded.
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let post_slug = PostSlug::new("hello".to_string()).expect("post slug");
    assert_eq!(
        store
            .get_registered_room(&site_id, &post_slug)
            .await
            .unwrap(),
        Some(replacement)
    );
    assert_eq!(
        store.get_room_status("!old:hs").await.unwrap(),
        Some(RoomStatus::Superseded)
    );
}

#[tokio::test]
async fn upgrade_comment_room_reuses_an_existing_replacement() {
    let (store, driver, site_service) = test_fixture("idempotent").await;
    driver.room_state.lock().await.insert(
        (
            "!old:hs".to_string(),
            "m.room.tombstone".to_string(),
            String::new(),
        ),
        json!({ "replacement_room": "!already-upgraded:hs" }),
    );

    let replacement = upgrade_comment_room(&driver, &store, &site_service, "!old:hs", "12")
        .await
        .expect("idempotent upgrade must succeed");
    assert_eq!(replacement, "!already-upgraded:hs");

    // The driver still recorded the call, but the homeserver tombstone won:
    // no second replacement room was minted.
    assert_eq!(
        *driver.upgrades.lock().await,
        vec![("!old:hs".to_string(), "12".to_string())]
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let post_slug = PostSlug::new("hello".to_string()).expect("post slug");
    assert_eq!(
        store
            .get_registered_room(&site_id, &post_slug)
            .await
            .unwrap(),
        Some("!already-upgraded:hs".to_string())
    );
}

#[tokio::test]
async fn upgrade_comment_room_rejects_unknown_rooms_and_bad_versions() {
    let (store, driver, site_service) = test_fixture("reject").await;

    let error = upgrade_comment_room(&driver, &store, &site_service, "!unknown:hs", "12")
        .await
        .expect_err("unknown room must be rejected");
    assert!(matches!(error, ManagementError::RoomNotRegistered(_)));

    let error = upgrade_comment_room(&driver, &store, &site_service, "!old:hs", "bad version!")
        .await
        .expect_err("invalid version must be rejected");
    assert!(matches!(error, ManagementError::InvalidRoomVersion(_)));
}

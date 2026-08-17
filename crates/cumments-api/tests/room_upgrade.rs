//! Integration tests for the shared room-upgrade management use case.
//!
//! The use case lives in `cumments-core` and is shared by the CLI, API and
//! bot; these tests drive it with a real registry/site store and the
//! in-memory Matrix driver so every convergence write is asserted.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use cumments_api::{ApiState, pow::Pow, rate_limit::RateLimiter};
use cumments_core::management::{ManagementError, upgrade_comment_room, upgrade_site_page_room};
use cumments_core::models::{PageSlug, RoomStatus, Site, SiteId};
use cumments_core::ports::{RegistryStore, SiteAuthStore, SiteStore};
use cumments_core::site_auth::{SiteAuthPolicy, SiteVerificationPolicy, token_hash};
use cumments_core::site_service::SiteService;
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

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
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_room("!old:hs", &site_id, &page_slug)
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
    driver.room_state.lock().await.insert(
        (
            "!old:hs".to_string(),
            "m.room.create".to_string(),
            String::new(),
        ),
        json!({ "room_version": "12" }),
    );
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

    let replacement = upgrade_comment_room(&driver, &store, &site_service, "!old:hs", "13")
        .await
        .expect("upgrade must succeed");
    assert_eq!(replacement, "!upgraded-1:hs");

    // The native upgrade was requested with the explicit target version.
    assert_eq!(
        *driver.upgrades.lock().await,
        vec![("!old:hs".to_string(), "13".to_string())]
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
    assert_eq!(metadata["page_slug"], "hello");

    // Registry: the replacement is active and the old room is superseded.
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
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

    let replacement = upgrade_comment_room(&driver, &store, &site_service, "!old:hs", "13")
        .await
        .expect("idempotent upgrade must succeed");
    assert_eq!(replacement, "!already-upgraded:hs");

    // The driver still recorded the call, but the homeserver tombstone won:
    // no second replacement room was minted.
    assert_eq!(
        *driver.upgrades.lock().await,
        vec![("!old:hs".to_string(), "13".to_string())]
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
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

#[tokio::test]
async fn upgrade_comment_room_supports_pre_v12_rooms() {
    let (store, driver, site_service) = test_fixture("pre-v12").await;
    driver.room_state.lock().await.insert(
        (
            "!old:hs".to_string(),
            "m.room.create".to_string(),
            String::new(),
        ),
        json!({ "room_version": "11" }),
    );

    let replacement = upgrade_comment_room(&driver, &store, &site_service, "!old:hs", "12")
        .await
        .expect("pre-v12 room upgrade must succeed when the bot can tombstone");
    assert_eq!(replacement, "!upgraded-1:hs");
}

#[tokio::test]
async fn upgrade_comment_room_rejects_downgrade_and_same_version() {
    let (store, driver, site_service) = test_fixture("not-newer").await;

    let error = upgrade_comment_room(&driver, &store, &site_service, "!old:hs", "11")
        .await
        .expect_err("downgrade must be rejected");
    assert!(matches!(error, ManagementError::RoomVersionNotNewer(_, _)));

    let error = upgrade_comment_room(&driver, &store, &site_service, "!old:hs", "12")
        .await
        .expect_err("same-version upgrade must be rejected");
    assert!(matches!(error, ManagementError::RoomVersionNotNewer(_, _)));
}

#[tokio::test]
async fn upgrade_site_page_room_resolves_registry_then_upgrades() {
    let (store, driver, site_service) = test_fixture("site-post").await;
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");

    let replacement =
        upgrade_site_page_room(&driver, &store, &site_service, &site_id, &page_slug, "13")
            .await
            .expect("site-post upgrade must succeed");
    assert_eq!(replacement, "!upgraded-1:hs");
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
            .await
            .unwrap(),
        Some(replacement)
    );
}

#[tokio::test]
async fn page_room_retire_endpoint_requires_claim_token_and_marks_retired() {
    let store = DbStore::connect(&test_db_url("api-post-retire"))
        .await
        .expect("connect test database");
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");

    let app = cumments_api::build_router(api_state(TestDriver::new(), store.clone()));
    let uri = "/api/v1/sites/my-blog/pages/hello";

    let missing = app
        .clone()
        .oneshot(api_request(Method::DELETE, uri, None, ""))
        .await
        .expect("call router");
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    let ok = app
        .clone()
        .oneshot(api_request(Method::DELETE, uri, Some("claim"), ""))
        .await
        .expect("call router");
    assert_eq!(ok.status(), StatusCode::OK);
    let body = axum::body::to_bytes(ok.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let data: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(data["status"], "retiring");
    assert_eq!(
        store
            .get_room_status("!room:hs")
            .await
            .expect("room status"),
        Some(RoomStatus::Retired)
    );

    let again = app
        .oneshot(api_request(Method::DELETE, uri, Some("claim"), ""))
        .await
        .expect("call router");
    assert_eq!(
        again.status(),
        StatusCode::NOT_FOUND,
        "retiring an already-retired room is a 404"
    );
}

fn api_state(driver: TestDriver, store: DbStore) -> ApiState {
    let (event_bus, _) = tokio::sync::broadcast::channel(100);
    let site_service_store: Arc<dyn SiteStore> = Arc::new(store.clone());
    ApiState {
        store: Arc::new(store.clone()),
        driver: Arc::new(driver),
        site_service: Arc::new(SiteService::new(site_service_store)),
        pow: Arc::new(Pow::new("test-secret".to_string(), 1)),
        event_bus,
        submission_notify: Arc::new(tokio::sync::Notify::new()),
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        site_auth_policy: Arc::new(SiteAuthPolicy {
            verification: SiteVerificationPolicy::Disabled,
            sites: Default::default(),
        }),
        operator_token_hash: None,
        registration_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        verification_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        operator_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(60))),
        confirm_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        trusted_proxies: Arc::new(Default::default()),
        allow_private_verification_origins: true,
        write_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        sse_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        sse_reconnect: Arc::new(std::sync::Mutex::new(
            cumments_api::routes::sse::SseReconnectRegistry::default(),
        )),
        max_sse_connections: 100,
        active_sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        media_proxy: None,
        media_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        visitor_profile_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        public_read_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        governance_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        ephemeral_bus: tokio::sync::broadcast::channel(16).0,
        ephemeral_state: None,
    }
}

fn api_request(method: Method, uri: &str, claim_token: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = claim_token {
        builder = builder.header("X-Cumments-Claim-Token", token);
    }
    builder
        .body(Body::from(body.to_owned()))
        .expect("build request")
}

#[tokio::test]
async fn site_level_upgrade_endpoint_requires_claim_token_and_upgrades() {
    let store = DbStore::connect(&test_db_url("api-site-upgrade"))
        .await
        .expect("connect test database");
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .save_site(&Site {
            id: site_id.as_str().to_string(),
            matrix_space_id: "!space:hs".to_string(),
            display_name: Some("my-blog".to_string()),
            created_at: chrono::Utc::now(),
        })
        .await
        .expect("save site");
    store
        .register_room("!old:hs", &site_id, &page_slug)
        .await
        .expect("register room");

    let driver = TestDriver::new();
    driver.room_state.lock().await.insert(
        (
            "!old:hs".to_string(),
            "m.room.create".to_string(),
            String::new(),
        ),
        json!({ "room_version": "12" }),
    );
    driver.power_levels.lock().await.insert(
        "!space:hs".to_string(),
        json!({ "users": { "@owner:hs": 100 } }),
    );

    let app = cumments_api::build_router(api_state(driver, store.clone()));
    let uri = "/api/v1/sites/my-blog/pages/hello/upgrade";

    let missing = app
        .clone()
        .oneshot(api_request(
            Method::POST,
            uri,
            None,
            r#"{"new_version":"13"}"#,
        ))
        .await
        .expect("call router");
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    let ok = app
        .oneshot(api_request(
            Method::POST,
            uri,
            Some("claim"),
            r#"{"new_version":"13"}"#,
        ))
        .await
        .expect("call router");
    assert_eq!(ok.status(), StatusCode::OK);
    let body = axum::body::to_bytes(ok.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let data: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(data["replacement_room"], "!upgraded-1:hs");
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
            .await
            .unwrap(),
        Some("!upgraded-1:hs".to_string())
    );
}

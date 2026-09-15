//! Router-level integration tests for write-path site authentication, the
//! Operator API, and the well-known verification flow.

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
    middleware,
    routing::{get, post},
};
use cumments_api::{ApiState, pow::Pow, rate_limit::RateLimiter, site_auth::enforce_site_auth};
use cumments_core::governance::{NewRoleClaim, RoleEntry, SITE_ADMIN_LEVEL};
use cumments_core::identity::{
    derive_visitor_id_from_public_key, post_signature_message, signature_message,
};
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, Message, MessageStatus, PageSlug, SiteId, TextContent,
    TextStyle, VisitorProfile,
};
use cumments_core::ports::{
    GovernanceStore, MediaReferenceStore, MessageStore, RegistryStore, RoleClaimStore,
    SiteAuthStore, SiteStore, SiteTransferStore, StickerPackStore, SubmissionStore,
};
use cumments_core::site_auth::{
    Origin, SiteAuthPolicy, SiteVerificationPolicy, site_request_signature, token_hash,
};
use cumments_core::site_service::SiteService;
use cumments_core::sticker_packs::{
    StickerImage, StickerPack, StickerPackContent, StickerPackProjection,
};
use cumments_store::DbStore;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

fn test_db_url(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "cumments-api-test-{name}-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn test_state(
    name: &str,
    policy: SiteVerificationPolicy,
    operator_token: Option<&str>,
) -> (ApiState, DbStore) {
    test_state_with_driver(
        name,
        policy,
        operator_token,
        Arc::new(cumments_matrix::LoggingMatrixDriver),
    )
    .await
}

async fn test_state_with_driver(
    name: &str,
    policy: SiteVerificationPolicy,
    operator_token: Option<&str>,
    driver: Arc<dyn cumments_core::ports::MatrixDriver>,
) -> (ApiState, DbStore) {
    test_state_with_driver_and_claim_limit(name, policy, operator_token, driver, 1000).await
}

async fn test_state_with_driver_and_claim_limit(
    name: &str,
    policy: SiteVerificationPolicy,
    operator_token: Option<&str>,
    driver: Arc<dyn cumments_core::ports::MatrixDriver>,
    claim_token_requests: usize,
) -> (ApiState, DbStore) {
    let store = DbStore::connect(&test_db_url(name))
        .await
        .expect("connect test database");
    let (event_bus, _) = tokio::sync::broadcast::channel(100);
    let site_service_store: Arc<dyn cumments_core::ports::SiteStore> = Arc::new(store.clone());
    let state = ApiState {
        store: Arc::new(store.clone()),
        driver,
        site_service: Arc::new(SiteService::new(site_service_store)),
        pow: Arc::new(Pow::new("test-secret".to_string(), 1)),
        event_bus,
        submission_notify: Arc::new(tokio::sync::Notify::new()),
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        site_auth_policy: Arc::new(SiteAuthPolicy {
            verification: policy,
            sites: Default::default(),
        }),
        operator_token_hash: operator_token.map(token_hash),
        server_name: Some("hs".to_string()),
        registration_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        verification_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        operator_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(60))),
        claim_token_limiter: Arc::new(RateLimiter::new(
            claim_token_requests,
            Duration::from_secs(60),
        )),
        confirm_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        trusted_proxies: Arc::new(Default::default()),
        // The existing integration test verifies against 127.0.0.1.
        allow_private_verification_origins: true,
        write_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        sse_limiter: Arc::new(cumments_api::rate_limit::SseRateLimiter::new(
            1000,
            Duration::from_secs(3600),
            100,
        )),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(100)),
        media_proxy: None,
        media_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        visitor_profile_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        public_read_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        governance_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        ephemeral_bus: tokio::sync::broadcast::channel(16).0,
        ephemeral_state: None,
        operation_locks: cumments_api::OperationLocks::new(),
    };
    (state, store)
}

fn middleware_router(state: ApiState) -> Router {
    async fn ok_handler() -> StatusCode {
        StatusCode::OK
    }
    Router::new()
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/comments",
            post(ok_handler).fallback(ok_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}",
            post(ok_handler).patch(ok_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/pages/{page_slug}/media",
            post(ok_handler).fallback(ok_handler),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            enforce_site_auth,
        ))
        .with_state(state)
}

fn request(
    method: Method,
    uri: &str,
    origin: Option<&str>,
    headers: &[(&str, String)],
) -> Request<Body> {
    request_with_body(method, uri, origin, headers, "{}")
}

fn request_with_body(
    method: Method,
    uri: &str,
    origin: Option<&str>,
    headers: &[(&str, String)],
    body: &str,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    for (name, value) in headers {
        builder = builder.header(*name, value.as_str());
    }
    let mut req = builder
        .body(Body::from(body.to_owned()))
        .expect("build request");
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:45678".parse::<SocketAddr>().unwrap(),
    ));
    req
}

fn query_method() -> Method {
    Method::from_bytes(b"QUERY").unwrap()
}

fn solve_pow(challenge: &cumments_api::pow::Challenge) -> String {
    use sha2::{Digest, Sha256};
    let mut nonce = 0u64;
    loop {
        let input = format!("{}{}", challenge.prefix, nonce);
        let hash = Sha256::digest(input.as_bytes());
        if hex::encode(hash).starts_with(&"0".repeat(challenge.difficulty as usize)) {
            return format!("{}|{}", challenge.prefix, nonce);
        }
        nonce += 1;
    }
}

fn response_origin(response: &axum::response::Response) -> Option<String> {
    response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

async fn body_text(response: axum::response::Response) -> String {
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&body).into_owned()
}

#[tokio::test]
async fn write_enforcement_follows_policy_and_origin() {
    // disabled: any origin passes, response echoes it
    let (state, store) = test_state("disabled", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = middleware_router(state);
    let response = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("https://any.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_origin(&response).as_deref(),
        Some("https://any.example.com")
    );

    // disabled also allows opaque null origins (file:// demo pages) and must
    // give the browser a wildcard CORS header so it can read the response.
    let null_origin = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(null_origin.status(), StatusCode::OK);
    assert_eq!(response_origin(&null_origin).as_deref(), Some("*"));

    // optional: unverified sites keep working
    let (state, store) = test_state("optional", SiteVerificationPolicy::Optional, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = middleware_router(state);
    let response = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("https://any.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::OK);

    // optional still rejects opaque origins (CVE-2026-27978 hardening).
    let null_optional = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(null_optional.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(null_optional)
            .await
            .contains("site-origin-denied")
    );

    // required: unknown sites are rejected with verification guidance
    let (state, store) = test_state("required", SiteVerificationPolicy::Required, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = middleware_router(state);
    let response = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("https://any.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(response)
            .await
            .contains("site-verification-required")
    );
}

#[tokio::test]
async fn visitor_profile_returns_the_current_profile_and_visitor_id() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let visitor_id = derive_visitor_id_from_public_key(&public_key).expect("visitor id");
    let driver = TestDriver::new().with_visitor_profile(
        "test-blog",
        public_key.clone(),
        VisitorProfile {
            display_name: Some("Alice".to_string()),
            avatar_url: Some("mxc://hs/avatar".to_string()),
        },
    );
    let (state, store) = test_state_with_driver(
        "visitor-profile",
        SiteVerificationPolicy::Disabled,
        None,
        Arc::new(driver),
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");

    let media_ref = store
        .get_or_create_reference(
            &SiteId::new("test-blog".to_string()).unwrap(),
            "mxc://hs/avatar",
            cumments_core::media_reference::MediaReferenceSource::Cumments,
        )
        .await
        .expect("create media ref");

    let router = cumments_api::build_router(state.clone());
    let uri = format!("/api/v1/sites/test-blog/visitors/profile?author_public_key={public_key}");
    let response = router
        .clone()
        .oneshot(request_with_body(Method::GET, &uri, None, &[], "null"))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_str(&body_text(response).await).expect("parse profile");
    assert_eq!(body["visitor_id"], visitor_id);
    assert_eq!(body["display_name"], "Alice");
    assert_eq!(body["avatar"], media_ref.as_str());
    // The media proxy is disabled in tests, so avatar_url is null (never exposes raw MXC).
    assert!(body["avatar_url"].is_null());
}

#[tokio::test]
async fn visitor_profile_returns_empty_profile_for_unknown_visitor() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let signing_key = SigningKey::from_bytes(&[9u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let visitor_id = derive_visitor_id_from_public_key(&public_key).expect("visitor id");
    let (state, store) = test_state_with_driver(
        "visitor-profile-empty",
        SiteVerificationPolicy::Disabled,
        None,
        Arc::new(TestDriver::new()),
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");

    let router = cumments_api::build_router(state.clone());
    let uri = format!("/api/v1/sites/test-blog/visitors/profile?author_public_key={public_key}");
    let response = router
        .clone()
        .oneshot(request_with_body(Method::GET, &uri, None, &[], "null"))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_str(&body_text(response).await).expect("parse profile");
    assert_eq!(body["visitor_id"], visitor_id);
    assert!(body["display_name"].is_null());
    assert!(body["avatar_url"].is_null());
}

#[tokio::test]
async fn visitor_profile_rejects_an_invalid_public_key() {
    let (state, store) = test_state(
        "visitor-profile-invalid",
        SiteVerificationPolicy::Disabled,
        None,
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");

    let router = cumments_api::build_router(state.clone());
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::GET,
            "/api/v1/sites/test-blog/visitors/profile?author_public_key=not-a-key",
            None,
            &[],
            "null",
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn public_reads_return_404_for_unregistered_sites() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::SigningKey;

    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let (state, _store) =
        test_state("public-read-404", SiteVerificationPolicy::Disabled, None).await;
    let router = cumments_api::build_router(state.clone());

    let query_method = Method::from_bytes(b"QUERY").expect("QUERY method");
    let requests: Vec<(Method, String, String)> = vec![
        (
            query_method,
            "/api/v1/sites/ghost/pages/hello/comments".to_string(),
            "{}".to_string(),
        ),
        (
            Method::GET,
            "/api/v1/sites/ghost/roles".to_string(),
            "null".to_string(),
        ),
        (
            Method::GET,
            "/api/v1/sites/ghost/stickers".to_string(),
            "null".to_string(),
        ),
        (
            Method::GET,
            format!("/api/v1/sites/ghost/visitors/profile?author_public_key={public_key}"),
            "null".to_string(),
        ),
    ];
    for (method, uri, body) in requests {
        let response = router
            .clone()
            .oneshot(request_with_body(method, &uri, None, &[], &body))
            .await
            .expect("call router");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{}", uri);
    }
}

#[tokio::test]
async fn operator_room_retire_mirror_marks_retired() {
    let (state, store) = test_state(
        "operator-room-retire",
        SiteVerificationPolicy::Disabled,
        Some("test-operator-token"),
    )
    .await;
    store
        .register_site("my-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");

    let router = cumments_api::build_router(state.clone());
    let uri = "/api/v1/operator/rooms/!room:hs/retirement";

    let missing = router
        .clone()
        .oneshot(request_with_body(Method::POST, uri, None, &[], ""))
        .await
        .expect("call router");
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    let ok = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            uri,
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
            "",
        ))
        .await
        .expect("call router");
    assert_eq!(ok.status(), StatusCode::ACCEPTED);
    let body = axum::body::to_bytes(ok.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let data: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(data["target_id"], "!room:hs");
    assert_eq!(data["state"], "retiring");
    assert_eq!(
        store
            .get_room_status("!room:hs")
            .await
            .expect("room status"),
        Some(cumments_core::models::RoomStatus::Retired)
    );
}

#[tokio::test]
async fn disabled_write_errors_include_wildcard_cors() {
    let (state, store) =
        test_state("disabled-errors", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = middleware_router(state);

    // Early validation failures must still be readable by the browser.
    let invalid_site = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/Bad-Site/pages/hello/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(invalid_site.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response_origin(&invalid_site).as_deref(), Some("*"));

    // In `disabled` mode the middleware deliberately does not buffer write
    // bodies (visitor media uploads keep the handler's 20MB cap instead of a
    // 1MB middleware cap), so a large body passes through to the handler and
    // the handler response still carries wildcard CORS.
    let oversized = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sites/test-blog/pages/hello/comments")
        .header(header::ORIGIN, "null")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("x".repeat(1024 * 1024 + 1)))
        .expect("build oversized request");
    let oversized = router
        .clone()
        .oneshot(oversized)
        .await
        .expect("call router");
    assert_eq!(oversized.status(), StatusCode::OK);
    assert_eq!(response_origin(&oversized).as_deref(), Some("*"));
}

#[tokio::test]
async fn comment_collection_rejects_resource_mutations() {
    let (state, store) = test_state(
        "comment-collection-mutations",
        SiteVerificationPolicy::Disabled,
        None,
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);

    let delete = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(delete.status(), StatusCode::METHOD_NOT_ALLOWED);

    let patch = router
        .clone()
        .oneshot(request(
            Method::PATCH,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(patch.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn verified_site_enforces_exact_origins_and_rejects_null() {
    let (state, store) = test_state("verified", SiteVerificationPolicy::Required, None).await;
    store
        .register_site(
            "a1b2c3d4e5f60718a1b2c3d4e5f60718",
            &token_hash("claim"),
            false,
        )
        .await
        .expect("register site");
    store
        .add_verified_origin(
            "a1b2c3d4e5f60718a1b2c3d4e5f60718",
            &Origin::parse("https://blog.example.com").unwrap(),
        )
        .await
        .expect("verify origin");
    let router = middleware_router(state);

    let allowed = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/pages/hello/comments",
            Some("https://blog.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(
        response_origin(&allowed).as_deref(),
        Some("https://blog.example.com")
    );

    let denied = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/pages/hello/comments",
            Some("https://evil.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(body_text(denied).await.contains("site-origin-denied"));

    let null_origin = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/pages/hello/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(null_origin.status(), StatusCode::FORBIDDEN);
    assert!(body_text(null_origin).await.contains("site-origin-denied"));

    let missing = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/pages/hello/comments",
            None,
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn secret_mode_requires_a_valid_hmac_signature() {
    let (state, store) = test_state("secret", SiteVerificationPolicy::Required, None).await;
    let site_id = "b2c3d4e5f60718a1b2c3d4e5f60718a1";
    store
        .register_site(site_id, &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .store_site_secret(site_id, "super-secret-hmac-key")
        .await
        .expect("store secret");
    let router = middleware_router(state);
    let uri = format!("/api/v1/sites/{site_id}/pages/hello/comments");

    let timestamp = chrono::Utc::now().timestamp().to_string();
    let signature =
        site_request_signature(b"super-secret-hmac-key", &timestamp, "POST", &uri, b"{}");
    let ok = router
        .clone()
        .oneshot(request(
            Method::POST,
            &uri,
            None,
            &[
                ("x-cumments-timestamp", timestamp.clone()),
                ("x-cumments-signature", signature),
            ],
        ))
        .await
        .expect("call router");
    assert_eq!(ok.status(), StatusCode::OK);

    let bad = router
        .clone()
        .oneshot(request(
            Method::POST,
            &uri,
            None,
            &[("x-cumments-signature", "deadbeef".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(bad.status(), StatusCode::FORBIDDEN);
    assert!(body_text(bad).await.contains("site-signature-invalid"));

    // Secret-mode authorization must read the body for the HMAC, so the
    // generic 1MB body cap still applies on the comment write path.
    let oversized = Request::builder()
        .method(Method::POST)
        .uri(&uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("x".repeat(1024 * 1024 + 1)))
        .expect("build oversized request");
    let oversized = router
        .clone()
        .oneshot(oversized)
        .await
        .expect("call router");
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);

    // The media upload route keeps the handler's 20MB cap: a >1MB body with
    // a valid HMAC must reach the handler instead of being rejected by the
    // generic 1MB body limit.
    let media_uri = format!("/api/v1/sites/{site_id}/pages/hello/media");
    let media_body = "x".repeat(1024 * 1024 + 1);
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let signature = site_request_signature(
        b"super-secret-hmac-key",
        &timestamp,
        "POST",
        &media_uri,
        media_body.as_bytes(),
    );
    let media = Request::builder()
        .method(Method::POST)
        .uri(&media_uri)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header("x-cumments-timestamp", timestamp)
        .header("x-cumments-signature", signature)
        .body(Body::from(media_body))
        .expect("build media request");
    let media = router.clone().oneshot(media).await.expect("call router");
    assert_eq!(media.status(), StatusCode::OK);
}

#[tokio::test]
async fn preflight_and_queries_are_public() {
    let (state, _) = test_state("cors", SiteVerificationPolicy::Required, None).await;
    let router = middleware_router(state);

    let preflight = router
        .clone()
        .oneshot(request(
            Method::OPTIONS,
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/pages/hello/comments",
            Some("https://blog.example.com"),
            &[("access-control-request-method", "QUERY".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    assert_eq!(response_origin(&preflight).as_deref(), Some("*"));

    let read = router
        .clone()
        .oneshot(request(
            Method::QUERY,
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/pages/hello/comments",
            None,
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(response_origin(&read).as_deref(), Some("*"));
}

#[tokio::test]
async fn sse_for_unregistered_page_returns_404_without_taking_stream_slot() {
    let (state, store) =
        test_state("sse-missing-room", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());

    let response = router
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/pages/hello/sse",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        state.sse_semaphore.available_permits(),
        100,
        "a missing room must not consume a concurrent stream slot"
    );
}

#[tokio::test]
async fn avatar_preflight_allows_put_and_delete() {
    let (state, store) = test_state("avatar-cors", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);

    for method in ["PUT", "DELETE"] {
        let preflight = router
            .clone()
            .oneshot(request(
                Method::OPTIONS,
                "/api/v1/sites/test-blog/visitors/profile/avatar",
                Some("null"),
                &[
                    ("access-control-request-method", method.to_string()),
                    (
                        "access-control-request-headers",
                        "content-type,idempotency-key".to_string(),
                    ),
                ],
            ))
            .await
            .expect("call router");
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        let allow = preflight
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .and_then(|v| v.to_str().ok())
            .expect("allow-methods header");
        assert!(
            allow.split(',').any(|m| m.trim() == method),
            "expected {method} in {allow}"
        );
    }
}

#[tokio::test]
async fn avatar_put_is_gated_by_site_auth() {
    // Origin mode: a disallowed origin is rejected before the handler.
    let (state, store) = test_state("avatar-origin", SiteVerificationPolicy::Required, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .add_verified_origin(
            "test-blog",
            &Origin::parse("https://blog.example.com").unwrap(),
        )
        .await
        .expect("verify origin");
    let router = cumments_api::build_router(state);

    let denied = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/test-blog/visitors/profile/avatar",
            Some("https://evil.example.com"),
            &[("idempotency-key", "avatar-origin-key".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(body_text(denied).await.contains("site-origin-denied"));

    let allowed = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/test-blog/visitors/profile/avatar",
            Some("https://blog.example.com"),
            &[("idempotency-key", "avatar-origin-key".to_string())],
        ))
        .await
        .expect("call router");
    assert_ne!(allowed.status(), StatusCode::FORBIDDEN);

    // Secret mode: an unsigned PUT is rejected before the handler.
    let (state, store) = test_state("avatar-secret", SiteVerificationPolicy::Required, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .store_site_secret("test-blog", "super-secret-hmac-key")
        .await
        .expect("store secret");
    let router = cumments_api::build_router(state);

    let unsigned = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/test-blog/visitors/profile/avatar",
            None,
            &[("idempotency-key", "avatar-secret-key".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);
    assert!(body_text(unsigned).await.contains("site-signature-invalid"));
}

#[tokio::test]
async fn avatar_put_requires_registered_site() {
    let (state, _) = test_state(
        "avatar-unregistered",
        SiteVerificationPolicy::Disabled,
        None,
    )
    .await;
    let router = cumments_api::build_router(state);

    let response = router
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/not-registered/visitors/profile/avatar",
            Some("null"),
            &[("idempotency-key", "avatar-unregistered-key".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(body_text(response).await.contains("site-not-registered"));
}

#[tokio::test]
async fn operator_lifecycle_and_well_known_verification() {
    let (state, _) = test_state(
        "operator",
        SiteVerificationPolicy::Required,
        Some("test-operator-token"),
    )
    .await;
    let router = cumments_api::build_router(state);

    // Register a site through the public API.
    let registered = router
        .clone()
        .oneshot(request(Method::POST, "/api/v1/sites", None, &[]))
        .await
        .expect("call router");
    assert_eq!(registered.status(), StatusCode::CREATED);
    let registered_json: serde_json::Value =
        serde_json::from_str(&body_text(registered).await).expect("parse response");
    let site_id = registered_json["site_id"].as_str().unwrap().to_string();
    let claim_token = registered_json["claim_token"].as_str().unwrap().to_string();

    // Operator list requires the token.
    let unauthorized = router
        .clone()
        .oneshot(request(query_method(), "/api/v1/operator/sites", None, &[]))
        .await
        .expect("call router");
    assert_eq!(unauthorized.status(), StatusCode::FORBIDDEN);

    let listed = router
        .clone()
        .oneshot(request(
            query_method(),
            "/api/v1/operator/sites",
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed_json: serde_json::Value =
        serde_json::from_str(&body_text(listed).await).expect("parse response");
    assert_eq!(listed_json["data"][0]["site_id"], site_id);
    assert_eq!(listed_json["data"][0]["auth_mode"], "origin");
    assert_eq!(listed_json["meta"]["total"], 1);
    assert_eq!(listed_json["meta"]["page"], 1);
    assert_eq!(listed_json["meta"]["total_pages"], 1);

    // Start verification for a local well-known endpoint and keep the
    // verification token from the response (not the claim token).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let origin = format!("http://{}", listener.local_addr().expect("local address"));

    let started = router
        .clone()
        .oneshot(
            request(
                Method::POST,
                &format!("/api/v1/sites/{site_id}/verifications"),
                None,
                &[("x-cumments-claim-token", claim_token.clone())],
            )
            .map(|_| {
                Body::from(
                    serde_json::json!({
                        "origins": [origin],
                        "methods": ["well-known"]
                    })
                    .to_string(),
                )
            }),
        )
        .await
        .expect("call router");
    let started_status = started.status();
    let started_body = body_text(started).await;
    assert_eq!(started_status, StatusCode::OK, "{}", started_body);
    let started_json: serde_json::Value =
        serde_json::from_str(&started_body).expect("parse start response");
    let verification_token = started_json["token"].as_str().unwrap().to_string();
    let origin = started_json["origins"][0].as_str().unwrap().to_string();

    // Publish the proof and confirm.
    let (site_id_for_server, token_for_server) = (site_id.clone(), verification_token.clone());
    tokio::spawn(async move {
        let body = serde_json::json!({
            "site_id": site_id_for_server,
            "token": token_for_server,
        })
        .to_string();
        let app = Router::new().route(
            "/.well-known/cumments.json",
            get(move || async move { body }),
        );
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve well-known");
    });

    let confirmed = router
        .clone()
        .oneshot(
            request(
                Method::POST,
                &format!("/api/v1/sites/{site_id}/verifications/confirm"),
                None,
                &[],
            )
            .map(|_| {
                Body::from(
                    serde_json::json!({
                        "origin": origin,
                        "token": verification_token,
                    })
                    .to_string(),
                )
            }),
        )
        .await
        .expect("call router");
    assert_eq!(
        confirmed.status(),
        StatusCode::OK,
        "{}",
        body_text(confirmed).await
    );

    // Operator: rotate, export a config snippet, revoke secret, revoke origin.
    let rotated = router
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/operator/sites/{site_id}/secret-rotations"),
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(
        rotated.status(),
        StatusCode::OK,
        "{}",
        body_text(rotated).await
    );

    let snippet = router
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/operator/sites/{site_id}/config-snippet"),
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(snippet.status(), StatusCode::OK);
    assert!(body_text(snippet).await.contains("auth_mode"));

    let revoked_secret = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            &format!("/api/v1/operator/sites/{site_id}/secret"),
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(revoked_secret.status(), StatusCode::OK);

    let revoked_origin = router
        .clone()
        .oneshot(
            request(
                Method::POST,
                &format!("/api/v1/operator/sites/{site_id}/origin-revocations"),
                None,
                &[("authorization", "Bearer test-operator-token".to_string())],
            )
            .map(|_| Body::from(serde_json::json!({ "origin": origin }).to_string())),
        )
        .await
        .expect("call router");
    assert_eq!(
        revoked_origin.status(),
        StatusCode::OK,
        "{}",
        body_text(revoked_origin).await
    );
}

#[tokio::test]
async fn private_verification_origins_rejected_by_default() {
    let (mut state, _) = test_state("private-origin", SiteVerificationPolicy::Disabled, None).await;
    state.allow_private_verification_origins = false;
    let router = cumments_api::build_router(state);

    let registered = router
        .clone()
        .oneshot(request(Method::POST, "/api/v1/sites", None, &[]))
        .await
        .expect("call router");
    assert_eq!(registered.status(), StatusCode::CREATED);
    let registered_json: serde_json::Value =
        serde_json::from_str(&body_text(registered).await).expect("parse response");
    let site_id = registered_json["site_id"].as_str().unwrap().to_string();
    let claim_token = registered_json["claim_token"].as_str().unwrap().to_string();

    let started = router
        .clone()
        .oneshot(
            request(
                Method::POST,
                &format!("/api/v1/sites/{site_id}/verifications"),
                None,
                &[("x-cumments-claim-token", claim_token)],
            )
            .map(|_| {
                Body::from(
                    serde_json::json!({
                        "origins": ["http://127.0.0.1:8080"],
                        "methods": ["well-known"]
                    })
                    .to_string(),
                )
            }),
        )
        .await
        .expect("call router");
    assert_eq!(started.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn operator_can_rotate_claim_token() {
    let (state, _) = test_state(
        "rotate-claim",
        SiteVerificationPolicy::Disabled,
        Some("token"),
    )
    .await;
    let router = cumments_api::build_router(state);

    let registered = router
        .clone()
        .oneshot(request(Method::POST, "/api/v1/sites", None, &[]))
        .await
        .expect("call router");
    let registered_json: serde_json::Value =
        serde_json::from_str(&body_text(registered).await).expect("parse response");
    let site_id = registered_json["site_id"].as_str().unwrap().to_string();
    let old_claim = registered_json["claim_token"].as_str().unwrap().to_string();

    let rotated = router
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/operator/sites/{site_id}/claim-token-rotations"),
            None,
            &[("authorization", "Bearer token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated_json: serde_json::Value =
        serde_json::from_str(&body_text(rotated).await).expect("parse response");
    let new_claim = rotated_json["claim_token"].as_str().unwrap().to_string();
    assert_ne!(new_claim, old_claim);

    // The new token works; the old one is rejected.
    let started = router
        .clone()
        .oneshot(
            request(
                Method::POST,
                &format!("/api/v1/sites/{site_id}/verifications"),
                None,
                &[("x-cumments-claim-token", new_claim)],
            )
            .map(|_| {
                Body::from(
                    serde_json::json!({
                        "origins": ["https://example.com"],
                        "methods": ["dns"]
                    })
                    .to_string(),
                )
            }),
        )
        .await
        .expect("call router");
    assert_eq!(started.status(), StatusCode::OK);

    let rejected = router
        .clone()
        .oneshot(
            request(
                Method::POST,
                &format!("/api/v1/sites/{site_id}/verifications"),
                None,
                &[("x-cumments-claim-token", old_claim)],
            )
            .map(|_| {
                Body::from(
                    serde_json::json!({
                        "origins": ["https://example.com"],
                        "methods": ["dns"]
                    })
                    .to_string(),
                )
            }),
        )
        .await
        .expect("call router");
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn operator_lists_quarantined_rooms() {
    let (state, store) = test_state(
        "quarantined-rooms",
        SiteVerificationPolicy::Disabled,
        Some("token"),
    )
    .await;
    let site = SiteId::from("my-blog");
    let slug = PageSlug::from("hello");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");
    store
        .quarantine_room("!room:hs", "Refusing to adopt room", 1, None)
        .await
        .expect("quarantine room");

    let router = cumments_api::build_router(state);
    let resp = router
        .clone()
        .oneshot(request(
            query_method(),
            "/api/v1/operator/quarantined-rooms",
            None,
            &[("authorization", "Bearer token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(resp.status(), StatusCode::OK);
    let json: serde_json::Value =
        serde_json::from_str(&body_text(resp).await).expect("parse response");
    assert_eq!(json["data"][0]["room_id"], "!room:hs");
    assert_eq!(json["data"][0]["site_id"], "my-blog");
    assert_eq!(json["meta"]["total"], 1);
    assert!(
        json["data"][0]["quarantine_reason"]
            .as_str()
            .unwrap()
            .contains("Refusing to adopt")
    );

    let filtered = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/operator/quarantined-rooms",
            None,
            &[("authorization", "Bearer token".to_string())],
            r#"{"site_id":"other"}"#,
        ))
        .await
        .expect("call router");
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered_json: serde_json::Value =
        serde_json::from_str(&body_text(filtered).await).expect("parse response");
    assert_eq!(filtered_json["data"].as_array().map(Vec::len), Some(0));
    assert_eq!(filtered_json["meta"]["total"], 0);

    // Reinstate is idempotent: 204 both times, and the list empties.
    let auth = [("authorization", "Bearer token".to_string())];
    let reinstated = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            "/api/v1/operator/quarantined-rooms/!room:hs",
            None,
            &auth,
        ))
        .await
        .expect("call router");
    assert_eq!(reinstated.status(), StatusCode::NO_CONTENT);

    let reinstated_again = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            "/api/v1/operator/quarantined-rooms/!room:hs",
            None,
            &auth,
        ))
        .await
        .expect("call router");
    assert_eq!(reinstated_again.status(), StatusCode::NO_CONTENT);

    let missing = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            "/api/v1/operator/quarantined-rooms/!unknown:hs",
            None,
            &auth,
        ))
        .await
        .expect("call router");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let after = router
        .clone()
        .oneshot(request(
            query_method(),
            "/api/v1/operator/quarantined-rooms",
            None,
            &auth,
        ))
        .await
        .expect("call router");
    let after_json: serde_json::Value =
        serde_json::from_str(&body_text(after).await).expect("parse response");
    assert_eq!(after_json["meta"]["total"], 0);
}

#[tokio::test]
async fn challenge_response_is_never_cached() {
    let (state, _) = test_state("challenge-cache", SiteVerificationPolicy::Disabled, None).await;
    let router = cumments_api::build_router(state);
    let response = router
        .clone()
        .oneshot(request(Method::GET, "/api/v1/challenge", None, &[]))
        .await
        .expect("call router");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        response
            .headers()
            .get(header::PRAGMA)
            .and_then(|value| value.to_str().ok()),
        Some("no-cache")
    );
}

#[tokio::test]
async fn location_posts_are_queued_and_idempotent() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    let (state, store) = test_state(
        "location-submission",
        SiteVerificationPolicy::Disabled,
        None,
    )
    .await;
    let site = SiteId::from("test-blog");
    let slug = PageSlug::from("hello");
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let message = signature_message(&[
        Some("LOCATE"),
        Some("test-blog"),
        Some("hello"),
        Some("geo:31.2,121.5"),
        None,
        None,
        Some(challenge.prefix.as_str()),
        Some("1"),
    ]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "geo_uri": "geo:31.2,121.5",
        "description": "here",
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string();

    let post = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/location",
            Some("null"),
            &[("idempotency-key", "locate-key-123456".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(post.status(), StatusCode::ACCEPTED);
    let post_text = body_text(post).await;
    let json: serde_json::Value = serde_json::from_str(&post_text).expect("json");
    let submission_id = json["submission_id"].as_i64().expect("submission_id");

    let pending = store
        .get_pending_post_submissions(10)
        .await
        .expect("pending submissions");
    assert_eq!(
        pending.len(),
        1,
        "location must be queued as a post submission"
    );
    assert_eq!(pending[0].id, submission_id);
    assert!(
        pending[0].command.location.is_some(),
        "submission must carry the location payload"
    );

    // Replays with the same key and body return the original submission without
    // consuming a new PoW challenge.
    let replayed = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/location",
            Some("null"),
            &[("idempotency-key", "locate-key-123456".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(replayed.status(), StatusCode::ACCEPTED);
    assert_eq!(
        replayed
            .headers()
            .get("idempotent-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let replayed_json: serde_json::Value =
        serde_json::from_str(&body_text(replayed).await).expect("json");
    assert_eq!(replayed_json["submission_id"].as_i64(), Some(submission_id));
}

#[tokio::test]
async fn comment_replay_returns_original_submission_without_consuming_pow() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    let (state, store) = test_state("comment-replay", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[9u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let message = post_signature_message(
        "test-blog",
        "hello",
        "hello world",
        None,
        None,
        &challenge.prefix,
    );
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "content": "hello world",
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string();

    let post = || {
        router.clone().oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[("idempotency-key", "comment-key-123456".to_string())],
            &body,
        ))
    };

    let first = post().await.expect("call router");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first_json: serde_json::Value =
        serde_json::from_str(&body_text(first).await).expect("json");
    let submission_id = first_json["submission_id"].as_i64().expect("submission_id");

    let second = post().await.expect("call router");
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    assert_eq!(
        second
            .headers()
            .get("idempotent-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let second_json: serde_json::Value =
        serde_json::from_str(&body_text(second).await).expect("json");
    assert_eq!(second_json["submission_id"].as_i64(), Some(submission_id));
    assert_eq!(
        store
            .get_pending_post_submissions(10)
            .await
            .expect("pending submissions")
            .len(),
        1,
        "replay must not queue a second submission"
    );
}

#[tokio::test]
async fn redacted_comment_reads_as_tombstone_and_rejects_new_reaction() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    let (state, store) =
        test_state("redacted-tombstone", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .register_room(
            "!room:hs",
            &SiteId::from("test-blog"),
            &PageSlug::from("hello"),
        )
        .await
        .expect("register room");

    let mut deleted = Message {
        event_id: "$deleted:hs".to_string(),
        site_id: "test-blog".to_string(),
        page_slug: "hello".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: Some("Alice".to_string()),
            avatar_url: None,
            media_reference: None,
            public_key: Some("visitor-key".to_string()),
            mxid: None,
        },
        content: Content::Text(TextContent {
            body: "deleted secret".to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        timestamp: chrono::Utc::now(),
        edited_at: None,
        reply_to: None,
        thread_root: None,
        submission_id: Some(42),
        status: MessageStatus::Redacted,
        redacted_at: Some(chrono::Utc::now()),
        redacted_by: Some("@moderator:hs".to_string()),
        reactions: Vec::new(),

        thread_summary: None,
        room_id: "!room:hs".to_string(),
        sender_mxid: "@_cumments_test-blog_abcd:hs".to_string(),
        matrix_event_type: "m.room.message".to_string(),
        raw_content: serde_json::json!({"body": "deleted secret"}),
    };
    // Simulate a live row written before R1 so the public contract test also
    // covers the stable shape served after projection/migration sanitization.
    deleted.content = Content::Text(TextContent {
        body: "deleted secret".to_string(),
        formatted_body: None,
        style: TextStyle::Normal,
    });
    store.save_message(&deleted).await.expect("save message");

    let router = cumments_api::build_router(state.clone());
    let query = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
            "",
        ))
        .await
        .expect("query comments");
    assert_eq!(query.status(), StatusCode::OK);
    let page: serde_json::Value = serde_json::from_str(&body_text(query).await).expect("json");
    assert_eq!(page["data"][0]["status"], "redacted");
    assert_eq!(page["data"][0]["content"]["type"], "redacted");
    assert!(
        !page.to_string().contains("deleted secret"),
        "redacted comment leaked original content: {page}"
    );

    let signing_key = SigningKey::from_bytes(&[8u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let challenge_response = solve_pow(&state.pow.generate_challenge());
    let challenge_prefix = challenge_response.split('|').next().expect("challenge");
    let signed_message = signature_message(&[
        Some("REACT"),
        Some("test-blog"),
        Some("hello"),
        Some("$deleted:hs"),
        Some("👍"),
        Some(challenge_prefix),
        Some("1"),
    ]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(signed_message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "key": "👍",
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string();

    let reaction = router
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments/$deleted%3Ahs/reactions",
            Some("null"),
            &[],
            &body,
        ))
        .await
        .expect("react to deleted comment");
    assert_eq!(reaction.status(), StatusCode::CONFLICT);
    assert!(
        body_text(reaction)
            .await
            .contains("The target comment has been deleted.")
    );
}

#[tokio::test]
async fn comment_media_must_reference_an_owned_upload() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    let (state, store) =
        test_state("media-ownership", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[13u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let media_url = "mxc://hs/cat";
    let message = post_signature_message(
        "test-blog",
        "hello",
        media_url,
        None,
        None,
        &challenge.prefix,
    );
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "content": "",
        "media": {
            "url": media_url,
            "filename": "cat.png",
            "mimetype": "image/png",
        },
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string();

    // No upload record yet: rejected.
    let denied = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[("idempotency-key", "media-key-123456".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(denied).await.contains("media must reference"),
        "unowned media must be rejected"
    );

    // After recording the upload for this author/site/post: accepted.
    store
        .record_media_upload(media_url, &public_key, "test-blog", Some("hello"))
        .await
        .expect("record upload");
    let accepted = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[("idempotency-key", "media-key-123456".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn claim_token_authentication_attempts_are_rate_limited() {
    let (state, store) = test_state_with_driver_and_claim_limit(
        "claim-auth-limit",
        SiteVerificationPolicy::Required,
        None,
        Arc::new(cumments_matrix::LoggingMatrixDriver),
        2,
    )
    .await;
    store
        .register_site("claim-limit", &token_hash("valid-token"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);
    let uri = "/api/v1/sites/claim-limit/admin-claims";
    let body = serde_json::json!({ "user_id": "@owner:hs" }).to_string();

    // Missing and invalid credentials both count against the same source-IP
    // admission bucket; they must not bypass it by varying the credential.
    for (token, expected) in [
        (None, StatusCode::FORBIDDEN),
        (Some("invalid-token".to_string()), StatusCode::FORBIDDEN),
        (
            Some("another-invalid-token".to_string()),
            StatusCode::TOO_MANY_REQUESTS,
        ),
    ] {
        let request = match token {
            None => request_with_body(Method::POST, uri, None, &[], &body),
            Some(value) => request_with_body(
                Method::POST,
                uri,
                None,
                &[("x-cumments-claim-token", value)],
                &body,
            ),
        };
        let response = router.clone().oneshot(request).await.expect("call router");
        assert_eq!(response.status(), expected);
    }
}

#[tokio::test]
async fn claim_token_is_checked_before_verification_origin_parsing() {
    let (state, store) = test_state(
        "claim-auth-before-origin",
        SiteVerificationPolicy::Optional,
        None,
    )
    .await;
    store
        .register_site("origin-order", &token_hash("valid-token"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);

    // If claim authentication ran inside the handler, this malformed origin
    // would become a 400 before the token was checked.
    let response = router
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/origin-order/verifications",
            None,
            &[("x-cumments-claim-token", "invalid-token".to_string())],
            &serde_json::json!({
                "origins": ["not-an-origin"],
                "methods": ["well-known"],
            })
            .to_string(),
        ))
        .await
        .expect("call router");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(body_text(response).await.contains("invalid claim token"));
}

#[tokio::test]
async fn issue_secret_claims_share_the_pre_auth_rate_limit() {
    let (state, store) = test_state_with_driver_and_claim_limit(
        "issue-secret-claim-limit",
        SiteVerificationPolicy::Optional,
        None,
        Arc::new(cumments_matrix::LoggingMatrixDriver),
        1,
    )
    .await;
    store
        .register_site("secret-limit", &token_hash("valid-token"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);
    let uri = "/api/v1/sites/secret-limit/secret";

    let missing = router
        .clone()
        .oneshot(request_with_body(Method::POST, uri, None, &[], "{}"))
        .await
        .expect("call router");
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    let invalid = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            uri,
            None,
            &[("x-cumments-claim-token", "invalid".to_string())],
            "{}",
        ))
        .await
        .expect("call router");
    assert_eq!(invalid.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn reaction_retries_do_not_require_idempotency_key_and_reuse_matrix_txn() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::{Signer, SigningKey};

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "reaction-natural-idempotency",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .register_room(
            "!room:hs",
            &SiteId::from("test-blog"),
            &PageSlug::from("hello"),
        )
        .await
        .expect("register room");
    store
        .save_message(&Message {
            event_id: "$comment:hs".to_string(),
            site_id: "test-blog".to_string(),
            page_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Matrix,
                display_name: None,
                avatar_url: None,
                media_reference: None,
                public_key: None,
                mxid: Some("@alice:hs".to_string()),
            },
            content: Content::Text(TextContent {
                body: "hello".to_string(),
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

            thread_summary: None,
            room_id: "!room:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            matrix_event_type: "m.room.message".to_string(),
            raw_content: serde_json::Value::Null,
        })
        .await
        .expect("save comment");

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[12u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let signed_message = signature_message(&[
        Some("REACT"),
        Some("test-blog"),
        Some("hello"),
        Some("$comment:hs"),
        Some("👍"),
        Some(challenge.prefix.as_str()),
        Some("1"),
    ]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(signed_message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "key": "👍",
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string();
    let uri = "/api/v1/sites/test-blog/pages/hello/comments/$comment%3Ahs/reactions";

    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            uri,
            Some("null"),
            &[],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let reactions = driver.reactions.lock().await.clone();
    assert_eq!(reactions.len(), 1);
    assert!(
        reactions[0].3.starts_with("cumments_react_"),
        "reaction must use a namespaced Matrix transaction ID"
    );

    // The PoW challenge is single-use, so an HTTP-level retry cannot create a
    // second reaction. A genuine network retry after the homeserver accepted
    // the first request is additionally protected by the deterministic txn ID.
    let retry = router
        .oneshot(request_with_body(
            Method::POST,
            uri,
            Some("null"),
            &[],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(retry.status(), StatusCode::FORBIDDEN);
    assert!(body_text(retry).await.contains("Proof-of-Work"));
    assert_eq!(driver.reactions.lock().await.len(), 1);
}

#[tokio::test]
async fn site_governance_roles_are_claim_token_scoped_and_projected() {
    let (state, store) = test_state(
        "governance",
        SiteVerificationPolicy::Required,
        Some("test-operator-token"),
    )
    .await;
    let site_id = "gov-flow-123";
    store
        .register_site(site_id, &token_hash("claim-token"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);

    let admin_uri = format!("/api/v1/sites/{site_id}/admin-claims");
    let admin_body = serde_json::json!({ "user_id": "@owner:hs" }).to_string();

    // Missing claim token is rejected before any Matrix write.
    let denied = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            &admin_uri,
            None,
            &[],
            &admin_body,
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    // With the claim token the API stores a pending claim and returns the
    // one-time verification token. No Matrix write happens yet.
    let added = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            &admin_uri,
            None,
            &[("x-cumments-claim-token", "claim-token".to_string())],
            &admin_body,
        ))
        .await
        .expect("call router");
    let added_json: serde_json::Value =
        serde_json::from_str(&body_text(added).await).expect("parse response");
    assert_eq!(added_json["pending"], serde_json::json!(true));
    assert_eq!(added_json["user_id"], "@owner:hs");
    assert_eq!(added_json["level"], 100);
    assert!(
        added_json["verify_token"]
            .as_str()
            .unwrap_or_default()
            .len()
            >= 32,
        "verification token must be returned once"
    );
    assert_eq!(
        store
            .pending_claims_for_user("@owner:hs")
            .await
            .expect("pending claims")
            .len(),
        1
    );

    // Nothing is provisioned before verification: the Space does not exist.
    let provisioned = store
        .get_site(&SiteId::new(site_id.to_string()).expect("valid site id"))
        .await
        .expect("load site")
        .expect("site exists");
    assert_eq!(provisioned.matrix_space_id, "");

    // Malformed and service-account user IDs are rejected up front.
    for (raw, label) in [
        ("@not-an-mxid", "garbage"),
        ("@_cumments_bot:hs", "as-account"),
    ] {
        let bad = router
            .clone()
            .oneshot(request_with_body(
                Method::POST,
                &admin_uri,
                None,
                &[("x-cumments-claim-token", "claim-token".to_string())],
                &serde_json::json!({ "user_id": raw }).to_string(),
            ))
            .await
            .expect("call router");
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST, "{label}");
    }

    // The operator mirror works without a claim token.
    let manager_uri = format!("/api/v1/operator/sites/{site_id}/manager-claims");
    let added_manager = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            &manager_uri,
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
            &serde_json::json!({ "user_id": "@manager:hs" }).to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(added_manager.status(), StatusCode::OK);
    let manager_json: serde_json::Value =
        serde_json::from_str(&body_text(added_manager).await).expect("parse response");
    assert_eq!(manager_json["pending"], serde_json::json!(true));
    assert_eq!(manager_json["level"], 75);

    // Deleting a pending claim revokes it without touching Matrix.
    let removed = router
        .clone()
        .oneshot(request_with_body(
            Method::DELETE,
            &format!("/api/v1/sites/{site_id}/admins/%40owner%3Ahs"),
            None,
            &[("x-cumments-claim-token", "claim-token".to_string())],
            "",
        ))
        .await
        .expect("call router");
    assert_eq!(removed.status(), StatusCode::OK);
    let removed_json: serde_json::Value =
        serde_json::from_str(&body_text(removed).await).expect("parse response");
    assert_eq!(removed_json["revoked"], serde_json::json!(true));
    assert!(
        store
            .pending_claims_for_user("@owner:hs")
            .await
            .expect("pending claims")
            .is_empty()
    );

    // GET reads the projected read model.
    store
        .replace_site_roles(
            site_id,
            &[
                RoleEntry {
                    user_id: "@owner:hs".into(),
                    level: 100,
                },
                RoleEntry {
                    user_id: "@manager:hs".into(),
                    level: 75,
                },
            ],
        )
        .await
        .expect("project roles");
    let listed = router
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/sites/{site_id}/roles"),
            None,
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed_json: serde_json::Value =
        serde_json::from_str(&body_text(listed).await).expect("parse response");
    assert_eq!(listed_json["admins"], serde_json::json!(["@owner:hs"]));
    assert_eq!(listed_json["managers"], serde_json::json!(["@manager:hs"]));
}

#[tokio::test]
async fn applied_admin_revocation_marks_the_claim_revoked() {
    let (state, store) = test_state(
        "applied-revoke",
        SiteVerificationPolicy::Required,
        Some("test-operator-token"),
    )
    .await;
    let site_id = "applied-revoke-site";
    store
        .register_site(site_id, &token_hash("claim-token"), false)
        .await
        .expect("register site");

    // Drive the claim to `applied` exactly like the DM + ClaimsPass flow.
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: site_id.to_string(),
            room_id: String::new(),
            user_id: "@owner:hs".to_string(),
            level: SITE_ADMIN_LEVEL,
            token_hash: "verify-hash".to_string(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        })
        .await
        .expect("upsert claim");
    let claim = store
        .pending_claims_for_user("@owner:hs")
        .await
        .expect("pending claims")
        .remove(0);
    assert!(store.mark_claim_activated(claim.id).await.unwrap());
    let activated = store
        .activated_unapplied_claims()
        .await
        .expect("activated claims")
        .remove(0);
    store
        .mark_claim_applied(activated.id)
        .await
        .expect("mark applied");
    store
        .replace_site_roles(
            site_id,
            &[RoleEntry {
                user_id: "@owner:hs".into(),
                level: SITE_ADMIN_LEVEL,
            }],
        )
        .await
        .expect("project admin");

    let router = cumments_api::build_router(state);
    let removed = router
        .clone()
        .oneshot(request_with_body(
            Method::DELETE,
            &format!("/api/v1/sites/{site_id}/admins/%40owner%3Ahs"),
            None,
            &[("x-cumments-claim-token", "claim-token".to_string())],
            "",
        ))
        .await
        .expect("call router");
    assert_eq!(removed.status(), StatusCode::OK);
    let removed_json: serde_json::Value =
        serde_json::from_str(&body_text(removed).await).expect("parse response");
    assert_eq!(removed_json["revoked"], serde_json::json!(true));
    assert!(
        store
            .list_applied_claims()
            .await
            .expect("applied claims")
            .is_empty(),
        "the applied claim row must be marked revoked after the Matrix write"
    );
}

#[tokio::test]
async fn ownership_transfer_starts_pending_claim_and_transfer() {
    let (state, store) = test_state(
        "ownership-transfer",
        SiteVerificationPolicy::Required,
        Some("test-operator-token"),
    )
    .await;
    let site_id = "transfer-site";
    store
        .register_site(site_id, &token_hash("claim-token"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);

    let started = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            &format!("/api/v1/sites/{site_id}/ownership-transfers"),
            None,
            &[("x-cumments-claim-token", "claim-token".to_string())],
            &serde_json::json!({ "user_id": "@new-owner:hs" }).to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(started.status(), StatusCode::OK);
    let started_json: serde_json::Value =
        serde_json::from_str(&body_text(started).await).expect("parse response");
    assert_eq!(started_json["pending"], serde_json::json!(true));
    assert_eq!(started_json["user_id"], "@new-owner:hs");
    assert_eq!(started_json["transfer"]["status"], "pending");
    assert_eq!(started_json["transfer"]["target_mxid"], "@new-owner:hs");
    assert_eq!(
        store
            .pending_claims_for_user("@new-owner:hs")
            .await
            .expect("pending claims")
            .len(),
        1
    );
    let transfer = store
        .find_pending_transfer(site_id)
        .await
        .expect("pending transfer")
        .expect("transfer exists");
    assert_eq!(transfer.target_mxid, "@new-owner:hs");

    // Re-issuing for a different target revokes the previous pending claim.
    let restarted = router
        .oneshot(request_with_body(
            Method::POST,
            &format!("/api/v1/sites/{site_id}/ownership-transfers"),
            None,
            &[("x-cumments-claim-token", "claim-token".to_string())],
            &serde_json::json!({ "user_id": "@other:hs" }).to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(restarted.status(), StatusCode::OK);
    assert!(
        store
            .pending_claims_for_user("@new-owner:hs")
            .await
            .expect("old pending claims")
            .is_empty()
    );
    assert_eq!(
        store
            .find_pending_transfer(site_id)
            .await
            .expect("pending transfer")
            .expect("transfer exists")
            .target_mxid,
        "@other:hs"
    );
}

#[tokio::test]
async fn page_roles_endpoint_returns_projected_ladder() {
    let (state, store) = test_state(
        "page-roles",
        SiteVerificationPolicy::Required,
        Some("test-operator-token"),
    )
    .await;
    let site_id = "page-roles-site";
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_site(site_id, &token_hash("claim-token"), false)
        .await
        .expect("register site");
    store
        .register_room(
            "!room:hs",
            &SiteId::new(site_id.to_string()).unwrap(),
            &page_slug,
        )
        .await
        .expect("register room");
    store
        .replace_room_roles(
            "!room:hs",
            &[
                cumments_core::governance::RoleEntry {
                    user_id: "@admin:hs".into(),
                    level: 100,
                },
                cumments_core::governance::RoleEntry {
                    user_id: "@manager:hs".into(),
                    level: 75,
                },
                cumments_core::governance::RoleEntry {
                    user_id: "@mod:hs".into(),
                    level: 50,
                },
            ],
        )
        .await
        .expect("project room roles");
    let router = cumments_api::build_router(state);
    let listed = router
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/sites/{site_id}/pages/hello/roles"),
            None,
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(listed.status(), StatusCode::OK);
    let json: serde_json::Value =
        serde_json::from_str(&body_text(listed).await).expect("parse response");
    assert_eq!(json["admins"], serde_json::json!(["@admin:hs"]));
    assert_eq!(json["managers"], serde_json::json!(["@manager:hs"]));
    assert_eq!(json["moderators"], serde_json::json!(["@mod:hs"]));
}

#[tokio::test]
async fn site_owner_can_rotate_claim_token() {
    let (state, store) = test_state(
        "owner-rotate",
        SiteVerificationPolicy::Required,
        Some("test-operator-token"),
    )
    .await;
    let site_id = "owner-rotate-site";
    store
        .register_site(site_id, &token_hash("old-token"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);
    let rotated = router
        .oneshot(request_with_body(
            Method::POST,
            &format!("/api/v1/sites/{site_id}/claim-token-rotations"),
            None,
            &[("x-cumments-claim-token", "old-token".to_string())],
            "",
        ))
        .await
        .expect("call router");
    assert_eq!(rotated.status(), StatusCode::OK);
    let json: serde_json::Value =
        serde_json::from_str(&body_text(rotated).await).expect("parse response");
    assert_ne!(json["claim_token"], "old-token");
    assert_eq!(
        store
            .get_claim_token_hash(site_id)
            .await
            .expect("token hash")
            .expect("exists"),
        token_hash(json["claim_token"].as_str().unwrap())
    );
}

#[tokio::test]
async fn registration_supports_chosen_ids() {
    let (state, _) = test_state("register-id", SiteVerificationPolicy::Disabled, None).await;
    let router = cumments_api::build_router(state);

    // A chosen id round-trips and the claim token is returned once.
    let named = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites",
            None,
            &[],
            r#"{"site_id":"my-blog"}"#,
        ))
        .await
        .expect("call router");
    assert_eq!(named.status(), StatusCode::CREATED);
    let named_json: serde_json::Value =
        serde_json::from_str(&body_text(named).await).expect("parse response");
    assert_eq!(named_json["site_id"], "my-blog");
    assert!(
        named_json["claim_token"].as_str().unwrap_or_default().len() >= 32,
        "claim token must be returned"
    );

    // Chosen ids are first-come: a duplicate conflicts.
    let duplicate = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites",
            None,
            &[],
            r#"{"site_id":"my-blog"}"#,
        ))
        .await
        .expect("call router");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    // Invalid ids fail validation before touching the registry.
    let invalid = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites",
            None,
            &[],
            r#"{"site_id":"Bad_ID"}"#,
        ))
        .await
        .expect("call router");
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    // An omitted id still gets a random 32-character id.
    let random = router
        .clone()
        .oneshot(request(Method::POST, "/api/v1/sites", None, &[]))
        .await
        .expect("call router");
    assert_eq!(random.status(), StatusCode::CREATED);
    let random_json: serde_json::Value =
        serde_json::from_str(&body_text(random).await).expect("parse response");
    assert_eq!(
        random_json["site_id"].as_str().unwrap_or_default().len(),
        32
    );
}

#[tokio::test]
async fn unregistered_sites_cannot_write() {
    let (state, store) = test_state("register-gate", SiteVerificationPolicy::Disabled, None).await;
    let router = cumments_api::build_router(state);

    // Registered sites pass the middleware and reach the handler.
    store
        .register_site("reg-site", &token_hash("claim"), false)
        .await
        .expect("register site");
    let allowed = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/reg-site/pages/p1/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(allowed.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(allowed)
            .await
            .contains("idempotency-key-required"),
        "registered site must pass the middleware to the handler"
    );

    // Unknown ids are rejected even in the "disabled" policy, so no Matrix
    // Space is ever auto-created for them.
    let unknown = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/ghost-site/pages/p1/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert!(
        body_text(unknown).await.contains("site-not-registered"),
        "unknown sites must be rejected with the stable problem code"
    );
}

#[tokio::test]
async fn custom_named_sites_require_verification_in_optional_mode() {
    let (state, store) = test_state("custom-name", SiteVerificationPolicy::Optional, None).await;
    let router = cumments_api::build_router(state);

    // A caller-chosen id is unverified: writes are rejected until an origin
    // is proven, even though `optional` relaxes random-id sites.
    store
        .register_site("custom-blog", &token_hash("claim"), true)
        .await
        .expect("register custom site");
    let denied = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/custom-blog/pages/p1/comments",
            Some("https://blog.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(denied)
            .await
            .contains("site-verification-required")
    );

    // A server-generated id keeps the relaxed `optional` behavior.
    store
        .register_site(
            "a1b2c3d4e5f60718a1b2c3d4e5f60718",
            &token_hash("claim"),
            false,
        )
        .await
        .expect("register random site");
    let random_ok = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/pages/p1/comments",
            Some("https://blog.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(random_ok.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(random_ok)
            .await
            .contains("idempotency-key-required"),
        "random-id unverified sites must reach the handler in optional mode"
    );

    // Verifying an origin activates the chosen id.
    store
        .add_verified_origin(
            "custom-blog",
            &Origin::parse("https://blog.example.com").expect("valid origin"),
        )
        .await
        .expect("verify origin");
    let allowed = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/custom-blog/pages/p1/comments",
            Some("https://blog.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(allowed.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(allowed)
            .await
            .contains("idempotency-key-required"),
        "verified custom-named sites must reach the handler"
    );
}

#[tokio::test]
async fn optional_mode_rejects_orphan_rows_without_ownership_proof() {
    let (state, store) = test_state("orphan-site", SiteVerificationPolicy::Optional, None).await;
    // A row with a Space mapping but no claim token: what remains after the
    // operator removes a `[sites]` entry, or what backfill rebuilds.
    store
        .ensure_site_exists("orphan-blog", "!space:hs")
        .await
        .expect("ensure orphan site");
    let router = cumments_api::build_router(state);

    let write = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/orphan-blog/pages/p1/comments",
            Some("https://blog.example.com"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(write)
            .await
            .contains("site-verification-required"),
        "orphan rows must not enjoy the optional-mode relaxation"
    );
}

#[tokio::test]
async fn retiring_a_site_stops_writes_and_requires_auth() {
    let (state, store) = test_state(
        "retire-site",
        SiteVerificationPolicy::Disabled,
        Some("test-operator-token"),
    )
    .await;
    store
        .register_site("retire-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);

    // Missing claim token is rejected.
    let denied = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/retire-blog/retirement",
            None,
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    // The owner retires through the claim-token path.
    let retired = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/retire-blog/retirement",
            None,
            &[("x-cumments-claim-token", "claim".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(retired.status(), StatusCode::ACCEPTED);
    let retired_json: serde_json::Value =
        serde_json::from_str(&body_text(retired).await).expect("parse response");
    assert_eq!(retired_json["state"], "retiring");

    // Writes now fail with 410 site-retired, even in the disabled policy.
    let write = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/retire-blog/pages/p1/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(write.status(), StatusCode::GONE);
    assert!(body_text(write).await.contains("site-retired"));

    // The claim token was cleared, so a second retire attempt is unauthenticated.
    let again = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/retire-blog/retirement",
            None,
            &[("x-cumments-claim-token", "claim".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(again.status(), StatusCode::FORBIDDEN);

    // The operator mirror works for another site.
    store
        .register_site("operator-retire", &token_hash("claim"), false)
        .await
        .expect("register site");
    let operator = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/operator/sites/operator-retire/retirement",
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(operator.status(), StatusCode::ACCEPTED);
    assert!(body_text(operator).await.contains("retiring"));
}

#[tokio::test]
async fn sticker_packs_read_publicly_and_write_with_claim_token() {
    let (state, store) = test_state_with_driver(
        "sticker-packs",
        SiteVerificationPolicy::Disabled,
        None,
        Arc::new(cumments_test_utils::TestDriver::new()),
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .ensure_site_exists("test-blog", "!space:hs")
        .await
        .expect("attach space");
    let router = cumments_api::build_router(state.clone());

    // Public read starts empty.
    let listed = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/stickers",
            None,
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed_json: serde_json::Value =
        serde_json::from_str(&body_text(listed).await).expect("parse response");
    assert_eq!(listed_json["packs"], serde_json::json!([]));

    // Write requires the claim token.
    let denied = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/packs/default/stickers",
            None,
            &[],
            &serde_json::json!({"shortcode": "cat", "url": "mxc://hs/1"}).to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    // Claim-token write succeeds through the logging driver.
    let added = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/packs/default/stickers",
            None,
            &[("x-cumments-claim-token", "claim".to_string())],
            &serde_json::json!({"shortcode": "cat", "url": "mxc://hs/1"}).to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(added.status(), StatusCode::OK);

    // The projected pack is what the public read serves.
    store
        .save_site_pack(&StickerPackProjection {
            pack: StickerPack {
                room_id: "!space:hs".to_string(),
                site_id: "test-blog".to_string(),
                state_key: "default".to_string(),
                content: StickerPackContent {
                    display_name: Some("默认包".to_string()),
                    usage: vec!["sticker".to_string()],
                    images: vec![StickerImage {
                        shortcode: "cat".to_string(),
                        url: "mxc://hs/1".to_string(),
                        body: Some("a cat".to_string()),
                        info: None,
                    }],
                    ..Default::default()
                },
            },
            event_id: "$pack:hs".to_string(),
            sender: "@owner:hs".to_string(),
            origin_server_ts: 1,
        })
        .await
        .expect("save pack");
    let listed = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/stickers",
            None,
            &[],
        ))
        .await
        .expect("call router");
    let listed_json: serde_json::Value =
        serde_json::from_str(&body_text(listed).await).expect("parse response");
    assert_eq!(listed_json["packs"][0]["pack_id"], "default");
    assert_eq!(listed_json["packs"][0]["display_name"], "默认包");
    assert_eq!(listed_json["packs"][0]["images"][0]["shortcode"], "cat");
    assert_eq!(listed_json["packs"][0]["images"][0]["body"], "a cat");
    // No media proxy configured: raw mxc is the preview fallback.
    assert_eq!(
        listed_json["packs"][0]["images"][0]["proxy_url"],
        "mxc://hs/1"
    );

    // Remove with the claim token.
    let removed = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            "/api/v1/sites/test-blog/packs/default/stickers/cat",
            None,
            &[("x-cumments-claim-token", "claim".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(removed.status(), StatusCode::OK);
}

#[tokio::test]
async fn sticker_packs_operator_fallback_requires_operator_token() {
    let (state, store) = test_state(
        "sticker-packs-operator",
        SiteVerificationPolicy::Disabled,
        Some("op-token"),
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .ensure_site_exists("test-blog", "!space:hs")
        .await
        .expect("attach space");
    let router = cumments_api::build_router(state);

    let denied = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/operator/sites/test-blog/packs/default/stickers",
            None,
            &[],
            &serde_json::json!({"shortcode": "cat", "url": "mxc://hs/1"}).to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    let added = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/operator/sites/test-blog/packs/default/stickers",
            None,
            &[("authorization", "Bearer op-token".to_string())],
            &serde_json::json!({"shortcode": "cat", "url": "mxc://hs/1"}).to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(added.status(), StatusCode::OK);
}

#[tokio::test]
async fn comment_stickers_must_reference_the_sites_packs() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    let (state, store) =
        test_state("sticker-comment", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[17u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let media_url = "mxc://hs/cat";
    let message = post_signature_message(
        "test-blog",
        "hello",
        media_url,
        None,
        None,
        &challenge.prefix,
    );
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "content": "",
        "media": {
            "url": media_url,
            "kind": "sticker",
        },
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string();

    // Not in any projected pack: rejected.
    let denied = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[("idempotency-key", "sticker-key-123456".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(denied).await.contains("sticker must reference"),
        "sticker outside the site's packs must be rejected"
    );

    // After the pack is projected: accepted.
    store
        .save_site_pack(&StickerPackProjection {
            pack: StickerPack {
                room_id: "!space:hs".to_string(),
                site_id: "test-blog".to_string(),
                state_key: "default".to_string(),
                content: StickerPackContent {
                    usage: vec!["sticker".to_string()],
                    images: vec![StickerImage {
                        shortcode: "cat".to_string(),
                        url: media_url.to_string(),
                        body: Some("a cat".to_string()),
                        info: Some(serde_json::json!({
                            "mimetype": "image/png",
                            "size": 100,
                            "w": 512,
                            "h": 512,
                        })),
                    }],
                    ..Default::default()
                },
            },
            event_id: "$pack:hs".to_string(),
            sender: "@owner:hs".to_string(),
            origin_server_ts: 1,
        })
        .await
        .expect("save pack");
    let accepted = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[("idempotency-key", "sticker-key-123456".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn reaction_remove_is_idempotent_and_uses_deterministic_txn() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_core::models::Reaction;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::{Signer, SigningKey};

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "reaction-unreact",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .register_room(
            "!room:hs",
            &SiteId::from("test-blog"),
            &PageSlug::from("hello"),
        )
        .await
        .expect("register room");
    let comment_id = "$comment:hs";
    store
        .save_message(&Message {
            event_id: comment_id.to_string(),
            site_id: "test-blog".to_string(),
            page_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Visitor,
                display_name: Some("Alice".to_string()),
                avatar_url: None,
                media_reference: None,
                public_key: Some("owner-key".to_string()),
                mxid: None,
            },
            content: Content::Text(TextContent {
                body: "hello".to_string(),
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

            thread_summary: None,
            room_id: "!room:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            matrix_event_type: "m.room.message".to_string(),
            raw_content: serde_json::Value::Null,
        })
        .await
        .expect("save comment");

    let signing_key = SigningKey::from_bytes(&[22u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let visitor_id = derive_visitor_id_from_public_key(&public_key).expect("visitor id");
    let virtual_mxid = format!("@_cumments_test-blog_{}:hs", visitor_id);

    // Pre-project one reaction so unreact can resolve it.
    let reaction_event_id = "$reaction:hs";
    store
        .save_reaction(&Reaction {
            event_id: reaction_event_id.to_string(),
            message_event_id: comment_id.to_string(),
            sender_mxid: virtual_mxid.clone(),
            key: "👍".to_string(),
            origin_server_ts: 1,
            redacted_at: None,
        })
        .await
        .expect("save reaction");

    let router = cumments_api::build_router(state.clone());

    // First unreact: should redact via driver with deterministic txn.
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let chal_prefix = challenge.prefix.clone();
    let msg = signature_message(&[
        Some("UNREACT"),
        Some("test-blog"),
        Some("hello"),
        Some(comment_id),
        Some("👍"),
        Some(&chal_prefix),
    ]);
    let sig = URL_SAFE_NO_PAD.encode(signing_key.sign(msg.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "author_public_key": public_key,
        "author_signature": sig,
        "challenge_response": challenge_response,
    })
    .to_string();
    let uri = "/api/v1/sites/test-blog/pages/hello/comments/$comment%3Ahs/reactions/%F0%9F%91%8D";
    let resp = router
        .clone()
        .oneshot(request_with_body(
            Method::DELETE,
            uri,
            Some("null"),
            &[],
            &body,
        ))
        .await
        .expect("unreact");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let redactions = driver.redactions.lock().await.clone();
    assert_eq!(redactions.len(), 1);
    assert_eq!(redactions[0].0, "!room:hs");
    assert_eq!(redactions[0].1, reaction_event_id);
    assert!(
        redactions[0].2.starts_with("cumments_unreact_"),
        "unreact must use namespaced txnId"
    );

    // Second unreact with fresh PoW but same key: idempotent 204, no new driver call expected
    // because the reaction is still active in the store (test driver does not auto-redact the row),
    // the handler will find the same row again and call driver again. We accept either 1 or 2 calls,
    // but the response must be 204 and not leak existence.
    let challenge2 = state.pow.generate_challenge();
    let challenge_response2 = solve_pow(&challenge2);
    let msg2 = signature_message(&[
        Some("UNREACT"),
        Some("test-blog"),
        Some("hello"),
        Some(comment_id),
        Some("👍"),
        Some(&challenge2.prefix),
    ]);
    let sig2 = URL_SAFE_NO_PAD.encode(signing_key.sign(msg2.as_bytes()).to_bytes());
    let body2 = serde_json::json!({
        "author_public_key": public_key,
        "author_signature": sig2,
        "challenge_response": challenge_response2,
    })
    .to_string();
    let resp2 = router
        .clone()
        .oneshot(request_with_body(
            Method::DELETE,
            uri,
            Some("null"),
            &[],
            &body2,
        ))
        .await
        .expect("second unreact");
    assert_eq!(resp2.status(), StatusCode::NO_CONTENT);

    // Unreact with unknown key: also 204 idempotent (does not reveal absence).
    let challenge3 = state.pow.generate_challenge();
    let challenge_response3 = solve_pow(&challenge3);
    let msg3 = signature_message(&[
        Some("UNREACT"),
        Some("test-blog"),
        Some("hello"),
        Some(comment_id),
        Some("❤️"),
        Some(&challenge3.prefix),
    ]);
    let sig3 = URL_SAFE_NO_PAD.encode(signing_key.sign(msg3.as_bytes()).to_bytes());
    let body3 = serde_json::json!({
        "author_public_key": public_key,
        "author_signature": sig3,
        "challenge_response": challenge_response3,
    })
    .to_string();
    let uri_unknown =
        "/api/v1/sites/test-blog/pages/hello/comments/$comment%3Ahs/reactions/%E2%9D%A4%EF%B8%8F";
    let resp3 = router
        .oneshot(request_with_body(
            Method::DELETE,
            uri_unknown,
            Some("null"),
            &[],
            &body3,
        ))
        .await
        .expect("unknown key unreact");
    assert_eq!(resp3.status(), StatusCode::NO_CONTENT);
    // No additional redaction for unknown key.
    assert_eq!(
        driver.redactions.lock().await.len(),
        2,
        "unknown key must not trigger a redaction"
    );
}

#[tokio::test]
async fn thread_query_and_single_get_expose_the_semantic_read_model() {
    let (state, store) =
        test_state("thread-query-api", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .register_room(
            "!room:hs",
            &SiteId::from("test-blog"),
            &PageSlug::from("hello"),
        )
        .await
        .expect("register room");

    let base = chrono::Utc::now();
    let seed = |event_id: &str,
                reply_to: Option<String>,
                thread_root: Option<String>,
                ts: i64,
                status: MessageStatus| Message {
        event_id: event_id.to_string(),
        site_id: "test-blog".to_string(),
        page_slug: "hello".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: Some("Alice".to_string()),
            avatar_url: None,
            media_reference: None,
            public_key: Some("visitor-key".to_string()),
            mxid: None,
        },
        content: if status == MessageStatus::Redacted {
            Content::Redacted
        } else {
            Content::Text(TextContent {
                body: event_id.to_string(),
                formatted_body: None,
                style: TextStyle::Normal,
            })
        },
        matrix_event_type: "m.room.message".to_string(),
        timestamp: base + chrono::Duration::seconds(ts),
        edited_at: None,
        reply_to,
        thread_root,
        submission_id: None,
        status,
        redacted_at: (status == MessageStatus::Redacted)
            .then(|| base + chrono::Duration::seconds(ts)),
        redacted_by: (status == MessageStatus::Redacted).then(|| "@moderator:hs".to_string()),
        reactions: Vec::new(),
        thread_summary: None,
        room_id: "!room:hs".to_string(),
        sender_mxid: "@_cumments_test-blog_abcd:hs".to_string(),
        raw_content: serde_json::json!({}),
    };

    for message in [
        seed("$root:hs", None, None, 1, MessageStatus::Active),
        // Direct reply inside the Thread.
        seed(
            "$a:hs",
            Some("$root:hs".to_string()),
            Some("$root:hs".to_string()),
            2,
            MessageStatus::Active,
        ),
        // Matrix-native style member without a direct reply.
        seed(
            "$b:hs",
            None,
            Some("$root:hs".to_string()),
            3,
            MessageStatus::Active,
        ),
        // Redacted member: excluded from the active Thread collection.
        seed(
            "$red:hs",
            Some("$root:hs".to_string()),
            Some("$root:hs".to_string()),
            4,
            MessageStatus::Redacted,
        ),
        // A different Thread.
        seed("$other:hs", None, None, 9, MessageStatus::Active),
        seed(
            "$other_member:hs",
            Some("$other:hs".to_string()),
            Some("$other:hs".to_string()),
            5,
            MessageStatus::Active,
        ),
    ] {
        store.save_message(&message).await.expect("seed message");
    }

    let router = cumments_api::build_router(state.clone());

    // The Thread collection returns the active members only: the root, the
    // redacted member, the other Thread, and unrelated top-level messages
    // are all excluded. Membership comes from thread_root, not reply_to.
    let thread = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
            r#"{"thread_root":"$root:hs"}"#,
        ))
        .await
        .expect("thread query");
    assert_eq!(thread.status(), StatusCode::OK);
    let thread: serde_json::Value = serde_json::from_str(&body_text(thread).await).expect("json");
    assert_eq!(thread["meta"]["total"], 2);
    let mut ids: Vec<String> = thread["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|item| item["event_id"].as_str().expect("event id").to_string())
        .collect();
    ids.sort();
    assert_eq!(ids, ["$a:hs", "$b:hs"]);
    for item in thread["data"].as_array().expect("data array") {
        assert_eq!(item["thread_root"], "$root:hs");
        assert!(
            item.get("thread_summary").is_none(),
            "members never carry a ThreadSummary: {item}"
        );
    }
    let by_id = |id: &str| {
        thread["data"]
            .as_array()
            .expect("data array")
            .iter()
            .find(|item| item["event_id"] == id)
            .expect("thread member")
            .clone()
    };
    assert_eq!(by_id("$a:hs")["reply_to"], "$root:hs");
    assert!(
        by_id("$b:hs").get("reply_to").is_none(),
        "a member without a direct reply keeps reply_to unset"
    );

    // The page collection exposes derived summaries on roots, none on
    // members, and keeps its total unchanged (tombstones included).
    let page = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
            "",
        ))
        .await
        .expect("page query");
    assert_eq!(page.status(), StatusCode::OK);
    let page: serde_json::Value = serde_json::from_str(&body_text(page).await).expect("json");
    assert_eq!(page["meta"]["total"], 6);
    let page_item = |id: &str| {
        page["data"]
            .as_array()
            .expect("data array")
            .iter()
            .find(|item| item["event_id"] == id)
            .expect("page item")
            .clone()
    };
    assert_eq!(page_item("$root:hs")["thread_summary"]["num_replies"], 2);
    assert_eq!(
        page_item("$root:hs")["thread_summary"]["latest_reply"],
        "$b:hs"
    );
    assert_eq!(page_item("$other:hs")["thread_summary"]["num_replies"], 1);
    assert!(page_item("$a:hs").get("thread_summary").is_none());

    // The single GET returns the same public representation for the root,
    // including the derived summary.
    let root = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/pages/hello/comments/$root%3Ahs",
            Some("null"),
            &[],
        ))
        .await
        .expect("get root");
    assert_eq!(root.status(), StatusCode::OK);
    let root: serde_json::Value = serde_json::from_str(&body_text(root).await).expect("json");
    assert_eq!(root["event_id"], "$root:hs");
    assert_eq!(root["thread_summary"]["num_replies"], 2);
    assert_eq!(root["thread_summary"]["latest_reply"], "$b:hs");

    // A member is individually addressable but carries no summary.
    let member = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/pages/hello/comments/$b%3Ahs",
            Some("null"),
            &[],
        ))
        .await
        .expect("get member");
    assert_eq!(member.status(), StatusCode::OK);
    let member: serde_json::Value = serde_json::from_str(&body_text(member).await).expect("json");
    assert_eq!(member["event_id"], "$b:hs");
    assert!(member.get("thread_summary").is_none());

    // Redacted comments stay individually addressable and keep their
    // structural relation metadata.
    let redacted = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/pages/hello/comments/$red%3Ahs",
            Some("null"),
            &[],
        ))
        .await
        .expect("get redacted");
    assert_eq!(redacted.status(), StatusCode::OK);
    let redacted: serde_json::Value =
        serde_json::from_str(&body_text(redacted).await).expect("json");
    assert_eq!(redacted["status"], "redacted");
    assert_eq!(redacted["content"]["type"], "redacted");
    assert_eq!(redacted["reply_to"], "$root:hs");
    assert_eq!(redacted["thread_root"], "$root:hs");

    // Out-of-scope and missing comments are 404.
    let out_of_scope = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/pages/hello/comments/$missing%3Ahs",
            Some("null"),
            &[],
        ))
        .await
        .expect("get missing");
    assert_eq!(out_of_scope.status(), StatusCode::NOT_FOUND);

    let mut foreign = seed("$foreign:hs", None, None, 1, MessageStatus::Active);
    foreign.page_slug = "elsewhere".to_string();
    store.save_message(&foreign).await.expect("seed foreign");
    let wrong_page = router
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/pages/hello/comments/$foreign%3Ahs",
            Some("null"),
            &[],
        ))
        .await
        .expect("get cross-page");
    assert_eq!(wrong_page.status(), StatusCode::NOT_FOUND);
}

/// The creation API treats `thread_root` and `reply_to` as independently
/// meaningful inputs: each referenced event must satisfy the existing
/// site/page scope and creation eligibility rules on its own, and no rule
/// requires `reply_to == thread_root` or `reply_to.thread_root == thread_root`
/// (the deliberately-removed cross-relation restriction). The durable command
/// must record both values exactly as submitted, and the visitor signature
/// must distinguish every relation combination.
#[tokio::test]
async fn post_comment_accepts_all_relation_combinations_independently() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    let (state, store) = test_state("thread-create", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .register_room(
            "!room:hs",
            &SiteId::from("test-blog"),
            &PageSlug::from("hello"),
        )
        .await
        .expect("register room");

    // Seed two distinct active comments so both relation targets are known
    // and resolvable in scope: `$root:hs` is a Thread root candidate and
    // `$parent:hs` is an unrelated top-level comment that is NOT a member of
    // any Thread. Accepting `reply_to = $parent` together with
    // `thread_root = $root` is therefore the strongest form of the
    // independence guarantee: neither value is rewritten, rejected, or
    // derived from the other even though they disagree.
    let seed = |event_id: &str| Message {
        event_id: event_id.to_string(),
        site_id: "test-blog".to_string(),
        page_slug: "hello".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: Some("Alice".to_string()),
            avatar_url: None,
            media_reference: None,
            public_key: Some("visitor-key".to_string()),
            mxid: None,
        },
        content: Content::Text(TextContent {
            body: event_id.to_string(),
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
        sender_mxid: "@_cumments_test-blog_abcd:hs".to_string(),
        raw_content: serde_json::json!({}),
    };
    for event_id in ["$root:hs", "$parent:hs"] {
        store
            .save_message(&seed(event_id))
            .await
            .expect("seed target");
    }

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[19u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let combinations: [(&str, Option<&str>, Option<&str>); 4] = [
        ("no-relation", None, None),
        ("thread-only", None, Some("$root:hs")),
        ("reply-only", Some("$parent:hs"), None),
        ("thread-and-reply", Some("$parent:hs"), Some("$root:hs")),
    ];
    for (index, (name, reply_to, thread_root)) in combinations.iter().enumerate() {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let message = post_signature_message(
            "test-blog",
            "hello",
            "a reply",
            *reply_to,
            *thread_root,
            &challenge.prefix,
        );
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
        let mut body = serde_json::json!({
            "content": "a reply",
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });
        if let Some(reply_to) = reply_to {
            body["reply_to"] = serde_json::json!(reply_to);
        }
        if let Some(thread_root) = thread_root {
            body["thread_root"] = serde_json::json!(thread_root);
        }

        let response = router
            .clone()
            .oneshot(request_with_body(
                Method::POST,
                "/api/v1/sites/test-blog/pages/hello/comments",
                Some("null"),
                &[("idempotency-key", format!("thread-create-{name}-{index}"))],
                &body.to_string(),
            ))
            .await
            .expect("post comment");
        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "combination {name} must be accepted as-is"
        );
        let accepted: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("json");
        let submission_id = accepted["submission_id"]
            .as_i64()
            .expect("submission id in 202 body");

        // The durable command preserves both semantic values unchanged.
        let pending = store
            .get_pending_post_submissions(100)
            .await
            .expect("pending submissions");
        let command = pending
            .iter()
            .find(|submission| submission.id == submission_id)
            .map(|submission| &submission.command)
            .expect("202 submission queued");
        assert_eq!(
            command.reply_to.as_deref(),
            *reply_to,
            "reply_to must reach the durable command for {name}"
        );
        assert_eq!(
            command.thread_root.as_deref(),
            *thread_root,
            "thread_root must reach the durable command for {name}"
        );
    }

    // Cross-combination controls: a signature made for one relation state
    // must not authorize a request carrying a different relation state.
    /// (signed reply_to, signed thread_root, sent reply_to, sent thread_root)
    type SignedAndSent<'a> = (
        Option<&'a str>,
        Option<&'a str>,
        Option<&'a str>,
        Option<&'a str>,
    );
    let mismatched: [SignedAndSent; 2] = [
        // Signed without relations, submitted with a thread root.
        (None, None, None, Some("$root:hs")),
        // Signed with reply only, submitted with both relations.
        (
            Some("$parent:hs"),
            None,
            Some("$parent:hs"),
            Some("$root:hs"),
        ),
    ];
    for (index, (signed_reply, signed_thread, sent_reply, sent_thread)) in
        mismatched.iter().enumerate()
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let message = post_signature_message(
            "test-blog",
            "hello",
            "a reply",
            *signed_reply,
            *signed_thread,
            &challenge.prefix,
        );
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
        let mut body = serde_json::json!({
            "content": "a reply",
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });
        if let Some(sent_reply) = sent_reply {
            body["reply_to"] = serde_json::json!(sent_reply);
        }
        if let Some(sent_thread) = sent_thread {
            body["thread_root"] = serde_json::json!(sent_thread);
        }
        let response = router
            .clone()
            .oneshot(request_with_body(
                Method::POST,
                "/api/v1/sites/test-blog/pages/hello/comments",
                Some("null"),
                &[("idempotency-key", format!("thread-tamper-{index}"))],
                &body.to_string(),
            ))
            .await
            .expect("post comment");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "relation state diverging from the signed payload must be rejected ({index})"
        );
    }
}

// ── Create Poll (frozen semantic operation) ───────────────────────

/// Build a signed Create Poll HTTP body for the frozen `POLL` operation.
#[allow(clippy::too_many_arguments)]
fn signed_poll_body(
    signing_key: &ed25519_dalek::SigningKey,
    site: &str,
    page: &str,
    operation_id: &str,
    question: &str,
    answers: &[(&str, &str)],
    kind: cumments_core::poll::PollSemanticKind,
    max_selections: u64,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
    challenge_prefix: &str,
    challenge_response: &str,
) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_core::poll::{
        PollSemanticAnswer, poll_semantic_operation, poll_signature_envelope,
    };
    use ed25519_dalek::Signer;

    let semantic_answers: Vec<PollSemanticAnswer> = answers
        .iter()
        .map(|(id, text)| PollSemanticAnswer::new(*id, *text))
        .collect();
    let operation = poll_semantic_operation(
        site,
        page,
        reply_to,
        thread_root,
        question,
        &semantic_answers,
        kind,
        max_selections,
    );
    let envelope = poll_signature_envelope(&operation, operation_id, challenge_prefix);
    let signature = URL_SAFE_NO_PAD.encode(
        signing_key
            .sign(envelope.to_canonical_bytes().as_slice())
            .to_bytes(),
    );
    assert!(
        cumments_core::poll::verify_poll_signature(
            &URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
            &operation,
            operation_id,
            challenge_prefix,
            &signature,
        ),
        "test helper self-check: signature must verify"
    );
    let wire_answers: Vec<serde_json::Value> = answers
        .iter()
        .map(|(id, text)| serde_json::json!({ "id": id, "text": text }))
        .collect();
    serde_json::json!({
        "question": question,
        "answers": wire_answers,
        "kind": kind.as_str(),
        "max_selections": max_selections,
        "author_public_key": URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
        "author_signature": signature,
        "reply_to": reply_to,
        "thread_root": thread_root,
        "challenge_response": challenge_response,
    })
    .to_string()
}

/// The `Idempotency-Key` header value, which is also the signed
/// `operation_id`.
const POLL_KEY: &str = "poll-key-123456";

fn poll_key() -> Vec<(&'static str, String)> {
    vec![("idempotency-key", POLL_KEY.to_string())]
}

#[tokio::test]
async fn create_poll_accepts_valid_poll_and_queues_durable_submission() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-create", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[21u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "Which meeting time works best?",
        &[("slot-10am", "10:00 AM UTC"), ("slot-2pm", "2:00 PM UTC")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );

    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &body,
        ))
        .await
        .expect("call router");
    let status = response.status();
    let text = body_text(response).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {text}");
    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert!(json["submission_id"].as_i64().expect("submission id") > 0);

    // The durable submission stores the structured semantic payload, not
    // Matrix wire JSON, so the reconciler can rebuild the operation.
    let pending = store
        .get_pending_post_submissions(10)
        .await
        .expect("pending submissions");
    assert_eq!(pending.len(), 1);
    let poll = pending[0]
        .command
        .poll
        .as_ref()
        .expect("poll payload present");
    assert_eq!(poll.question, "Which meeting time works best?");
    assert_eq!(poll.answers.len(), 2);
    assert_eq!(poll.answers[0].id, "slot-10am");
    assert_eq!(poll.answers[1].id, "slot-2pm");
    assert_eq!(poll.kind, PollSemanticKind::Disclosed);
    assert_eq!(poll.max_selections, 1);
    assert_eq!(poll.operation_id, POLL_KEY);
}

#[tokio::test]
async fn create_poll_rejects_invalid_definitions() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-invalid", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[22u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);

    let post = |body: String| {
        router.clone().oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &body,
        ))
    };

    // Invalid answer-id syntax.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("has space", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::BAD_REQUEST
    );

    // Duplicate answer ids.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("a", "A again")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::BAD_REQUEST
    );

    // max_selections of zero is structurally invalid.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        0,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::BAD_REQUEST
    );

    // max_selections greater than the number of answers is semantically invalid.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        3,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::BAD_REQUEST
    );

    // A single answer is below the minimum.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::BAD_REQUEST
    );

    // None of the invalid attempts may queue work or consume the challenge.
    assert!(
        store
            .get_pending_post_submissions(10)
            .await
            .expect("pending")
            .is_empty(),
        "invalid definitions must not queue submissions"
    );
}

#[tokio::test]
async fn create_poll_replay_returns_original_without_consuming_pow() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-replay", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[23u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Undisclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    let post = || {
        router.clone().oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &body,
        ))
    };

    let first = post().await.expect("call router");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first_json: serde_json::Value =
        serde_json::from_str(&body_text(first).await).expect("json");
    let submission_id = first_json["submission_id"].as_i64().expect("submission id");

    let second = post().await.expect("call router");
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    assert_eq!(
        second
            .headers()
            .get("idempotent-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let second_json: serde_json::Value =
        serde_json::from_str(&body_text(second).await).expect("json");
    assert_eq!(second_json["submission_id"].as_i64(), Some(submission_id));
    assert_eq!(
        store
            .get_pending_post_submissions(10)
            .await
            .expect("pending")
            .len(),
        1,
        "a replay must not queue a second submission"
    );
}

#[tokio::test]
async fn create_poll_conflicts_on_fingerprint_or_author() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-conflict", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let alice = SigningKey::from_bytes(&[24u8; 32]);
    let mallory = SigningKey::from_bytes(&[25u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let post = |body: String| {
        router.clone().oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &body,
        ))
    };

    // First, a genuinely new operation.
    let body = signed_poll_body(
        &alice,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::ACCEPTED
    );

    // Same key, same author, different semantic fingerprint -> 409.
    let body = signed_poll_body(
        &alice,
        "test-blog",
        "hello",
        POLL_KEY,
        "different?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::CONFLICT
    );

    // Same key, different authenticated author -> 409 (server-wide non-reuse).
    let body = signed_poll_body(
        &mallory,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    let response = post(body).await.expect("call");
    assert_eq!(response.status(), StatusCode::CONFLICT);

    assert_eq!(
        store
            .get_pending_post_submissions(10)
            .await
            .expect("pending")
            .len(),
        1,
        "conflicts must not queue additional submissions"
    );
}

#[tokio::test]
async fn create_poll_signature_binds_the_semantic_operation() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-signature", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[26u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);

    // Sign the correct operation, then send a body whose question differs.
    let signed = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "original?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    let mut value: serde_json::Value = serde_json::from_str(&signed).unwrap();
    value["question"] = serde_json::json!("tampered?");
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &value.to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        store
            .get_pending_post_submissions(10)
            .await
            .expect("pending")
            .is_empty(),
        "an invalid signature must not queue work"
    );

    // The failed request must not have consumed the challenge: the correctly
    // signed request can still use it.
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &signed,
        ))
        .await
        .expect("call router");
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "signature failure must not consume PoW"
    );
}

#[tokio::test]
async fn create_poll_invalid_pow_is_rejected_for_a_new_operation() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-pow", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[27u8; 32]);
    let challenge = state.pow.generate_challenge();

    // Correct outer signature (over the real challenge), but no valid PoW.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        "not-a-valid-pow-response",
    );
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        store
            .get_pending_post_submissions(10)
            .await
            .expect("pending")
            .is_empty(),
        "a failed PoW must not queue work"
    );
}

#[tokio::test]
async fn create_poll_transport_formatting_does_not_affect_identity() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-format", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[28u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );

    let first = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(first.status(), StatusCode::ACCEPTED);

    // Re-order the JSON object keys and add whitespace; semantically identical.
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let reformatted = serde_json::to_string_pretty(&parsed).expect("pretty json");
    let second = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &reformatted,
        ))
        .await
        .expect("call router");
    assert_eq!(
        second.status(),
        StatusCode::ACCEPTED,
        "formatting differences must not become a conflict"
    );
    assert_eq!(
        second
            .headers()
            .get("idempotent-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
}

#[tokio::test]
async fn create_poll_accepts_independent_reply_and_thread_relations() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-relations", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[29u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);

    // Both relations are signed independently and accepted together.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        Some("$parent:hs"),
        Some("$thread:hs"),
        &challenge.prefix,
        &challenge_response,
    );
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let pending = store
        .get_pending_post_submissions(10)
        .await
        .expect("pending");
    assert_eq!(pending[0].command.reply_to.as_deref(), Some("$parent:hs"));
    assert_eq!(
        pending[0].command.thread_root.as_deref(),
        Some("$thread:hs")
    );

    // A malformed relation is rejected structurally.
    let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
    value["thread_root"] = serde_json::json!("not-an-event-id");
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &value.to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_poll_concurrent_same_key_creates_one_operation() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) =
        test_state("poll-concurrent", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[41u8; 32]);

    // Each concurrent attempt carries its own valid PoW challenge so the
    // single-use PoW primitive is not the thing under test here.
    let mut handles = Vec::new();
    for _ in 0..8 {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let body = signed_poll_body(
            &signing_key,
            "test-blog",
            "hello",
            POLL_KEY,
            "q?",
            &[("a", "A"), ("b", "B")],
            PollSemanticKind::Disclosed,
            1,
            None,
            None,
            &challenge.prefix,
            &challenge_response,
        );
        let router = router.clone();
        handles.push(tokio::spawn(async move {
            router
                .oneshot(request_with_body(
                    Method::POST,
                    "/api/v1/sites/test-blog/pages/hello/polls",
                    Some("null"),
                    &poll_key(),
                    &body,
                ))
                .await
                .expect("call router")
        }));
    }

    let mut statuses = Vec::new();
    let mut submission_ids = std::collections::HashSet::new();
    let mut replayed = 0;
    for handle in handles {
        let response = handle.await.expect("join");
        statuses.push(response.status());
        let is_replay = response.headers().get("idempotent-replayed").is_some();
        if is_replay {
            replayed += 1;
        }
        let json: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("json");
        submission_ids.insert(json["submission_id"].as_i64().expect("submission id"));
    }

    assert!(
        statuses
            .iter()
            .all(|status| *status == StatusCode::ACCEPTED),
        "same key/author/fingerprint must converge to 202, got {statuses:?}"
    );
    assert_eq!(
        submission_ids.len(),
        1,
        "all concurrent identical attempts must share one logical operation"
    );
    assert_eq!(replayed, 7, "exactly one attempt is fresh, the rest replay");

    // Exactly one server-wide operation and one durable submission.
    assert!(store.lookup_operation(POLL_KEY).await.unwrap().is_some());
    assert_eq!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .len(),
        1
    );
}

#[tokio::test]
async fn create_poll_failures_do_not_create_an_operation_claim() {
    use cumments_core::poll::PollSemanticKind;
    use ed25519_dalek::SigningKey;

    let (state, store) = test_state("poll-no-claim", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[42u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let post = |body: String| {
        router.clone().oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls",
            Some("null"),
            &poll_key(),
            &body,
        ))
    };

    // Semantically invalid definition.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("a", "dup")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::BAD_REQUEST
    );

    // Invalid signature.
    let signed = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        &challenge_response,
    );
    let mut value: serde_json::Value = serde_json::from_str(&signed).unwrap();
    value["question"] = serde_json::json!("tampered?");
    assert_eq!(
        post(value.to_string()).await.expect("call").status(),
        StatusCode::FORBIDDEN
    );

    // Invalid PoW.
    let body = signed_poll_body(
        &signing_key,
        "test-blog",
        "hello",
        POLL_KEY,
        "q?",
        &[("a", "A"), ("b", "B")],
        PollSemanticKind::Disclosed,
        1,
        None,
        None,
        &challenge.prefix,
        "not-a-pow",
    );
    assert_eq!(
        post(body).await.expect("call").status(),
        StatusCode::FORBIDDEN
    );

    // None of the failed attempts may have claimed the operation.
    assert!(
        store.lookup_operation(POLL_KEY).await.unwrap().is_none(),
        "failed requests must not create an operation claim"
    );
    assert!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .is_empty()
    );
}

// ── Vote ──────────────────────────────────────────────────────────

/// Seed a projected Poll-backed comment with the given ordered answers.
async fn seed_poll(store: &DbStore, poll_id: &str, options: &[(&str, &str)], max_selections: u8) {
    use cumments_core::models::{PollContent, PollOption};
    let site = SiteId::from("test-blog");
    let slug = PageSlug::from("hello");
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    store
        .register_room("!room:hs", &site, &slug)
        .await
        .expect("register room");
    store
        .save_message(&Message {
            event_id: poll_id.to_string(),
            site_id: "test-blog".to_string(),
            page_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Visitor,
                display_name: Some("Alice".to_string()),
                avatar_url: None,
                media_reference: None,
                public_key: Some("poll-author-key".to_string()),
                mxid: None,
            },
            content: Content::Poll(PollContent {
                question: "q?".to_string(),
                answers: options
                    .iter()
                    .map(|(id, text)| PollOption {
                        id: id.to_string(),
                        text: text.to_string(),
                    })
                    .collect(),
                kind: cumments_core::poll::PollSemanticKind::Disclosed,
                max_selections: u64::from(max_selections),
                status: cumments_core::poll::PollStatus::Open,
                end_time: None,
                results: None,
                total_votes: 0,
                responses: Vec::new(),
                my_votes: None,
            }),
            matrix_event_type: "org.matrix.msc3381.poll.start".to_string(),
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
            sender_mxid: "@_cumments_test-blog_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:hs".to_string(),
            raw_content: serde_json::json!({}),
        })
        .await
        .expect("seed poll");
}

/// Build a signed VOTE body for the frozen semantic operation.
#[allow(clippy::too_many_arguments)]
fn signed_vote_body(
    signing_key: &ed25519_dalek::SigningKey,
    site: &str,
    page: &str,
    poll_id: &str,
    operation_id: &str,
    raw_option_ids: &[&str],
    challenge_prefix: &str,
    challenge_response: &str,
) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_core::poll::{poll_signature_envelope, vote_semantic_operation};
    use ed25519_dalek::Signer;

    let mut canonical: Vec<String> = raw_option_ids.iter().map(|s| s.to_string()).collect();
    canonical.sort();
    canonical.dedup();
    let operation = vote_semantic_operation(site, page, poll_id, &canonical);
    let envelope = poll_signature_envelope(&operation, operation_id, challenge_prefix);
    let signature = URL_SAFE_NO_PAD.encode(
        signing_key
            .sign(envelope.to_canonical_bytes().as_slice())
            .to_bytes(),
    );
    serde_json::json!({
        "option_ids": raw_option_ids,
        "author_public_key": URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string()
}

fn vote_uri() -> &'static str {
    "/api/v1/sites/test-blog/pages/hello/polls/$poll:hs/votes"
}

fn vote_key() -> Vec<(&'static str, String)> {
    vec![("idempotency-key", "vote-op-1".to_string())]
}

#[tokio::test]
async fn vote_active_poll_emits_one_matrix_response() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-ok",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[61u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["b"],
        &challenge.prefix,
        &challenge_response,
    );

    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let recorded = driver.poll_responses.lock().await;
    assert_eq!(recorded.len(), 1, "exactly one poll response is emitted");
    assert_eq!(recorded[0].poll_event_id, "$poll:hs");
    assert_eq!(recorded[0].option_ids, vec!["b".to_string()]);
    assert_eq!(recorded[0].operation_id, "vote-op-1");
    assert!(!recorded[0].txn_id.is_empty());
    drop(recorded);

    // Vote is synchronous and creates no durable submission.
    assert!(
        store
            .get_pending_post_submissions(10)
            .await
            .expect("pending")
            .is_empty(),
        "a vote must not create a durable submission"
    );
}

#[tokio::test]
async fn vote_canonicalizes_duplicate_and_reordered_selections() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-canonical",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B"), ("c", "C")], 2).await;
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[62u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    // Duplicate + reordered input: canonical set is ["a","b"].
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["b", "a", "b"],
        &challenge.prefix,
        &challenge_response,
    );
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let recorded = driver.poll_responses.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].option_ids,
        vec!["a".to_string(), "b".to_string()],
        "the emitted answers must be the canonical sorted set"
    );
}

#[tokio::test]
async fn vote_rejects_invalid_unknown_and_excess_selections() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-invalid",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[63u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);

    let post = |option_ids: &[&str]| {
        let body = signed_vote_body(
            &signing_key,
            "test-blog",
            "hello",
            "$poll:hs",
            "vote-op-1",
            option_ids,
            &challenge.prefix,
            &challenge_response,
        );
        router.clone().oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &body,
        ))
    };

    assert_eq!(
        post(&["unknown"]).await.expect("call").status(),
        StatusCode::BAD_REQUEST,
        "unknown option id"
    );
    assert_eq!(
        post(&["a", "b"]).await.expect("call").status(),
        StatusCode::BAD_REQUEST,
        "excess selections (max_selections = 1)"
    );
    assert_eq!(
        post(&["has space"]).await.expect("call").status(),
        StatusCode::BAD_REQUEST,
        "invalid option id syntax"
    );

    assert!(
        driver.poll_responses.lock().await.is_empty(),
        "rejected votes must emit no Matrix response"
    );
    assert!(
        store.lookup_operation("vote-op-1").await.unwrap().is_none(),
        "rejected votes must not claim the operation"
    );
}

#[tokio::test]
async fn vote_empty_selection_is_an_unvote() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-unvote",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[64u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &[],
        &challenge.prefix,
        &challenge_response,
    );
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let recorded = driver.poll_responses.lock().await;
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0].option_ids.is_empty(),
        "unvote emits an empty answers array"
    );
}

#[tokio::test]
async fn vote_replay_consumes_no_pow_and_emits_no_second_event() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-replay",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[65u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );
    let post = || {
        router.clone().oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &body,
        ))
    };

    assert_eq!(
        post().await.expect("first").status(),
        StatusCode::NO_CONTENT
    );
    // Reusing the exact same single-use challenge proves the replay path never
    // reaches PoW verification.
    assert_eq!(
        post().await.expect("replay").status(),
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        driver.poll_responses.lock().await.len(),
        1,
        "a replay must not emit a second Matrix response"
    );
}

#[tokio::test]
async fn vote_conflicts_on_fingerprint_or_author() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-conflict",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());
    let alice = SigningKey::from_bytes(&[66u8; 32]);
    let mallory = SigningKey::from_bytes(&[67u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let post = |body: String| {
        router.clone().oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &body,
        ))
    };

    let first = signed_vote_body(
        &alice,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(first).await.expect("first").status(),
        StatusCode::NO_CONTENT
    );

    // Same key + same author + different fingerprint.
    let different_fp = signed_vote_body(
        &alice,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["b"],
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(different_fp).await.expect("fp conflict").status(),
        StatusCode::CONFLICT
    );

    // Same key + different author.
    let different_author = signed_vote_body(
        &mallory,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );
    assert_eq!(
        post(different_author)
            .await
            .expect("author conflict")
            .status(),
        StatusCode::CONFLICT
    );

    assert_eq!(
        driver.poll_responses.lock().await.len(),
        1,
        "conflicts must not emit another Matrix response"
    );
}

#[tokio::test]
async fn vote_signature_binds_target_and_selection() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-signature",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[68u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);

    // Sign ["a"] but send ["b"].
    let signed = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );
    let mut value: serde_json::Value = serde_json::from_str(&signed).unwrap();
    value["option_ids"] = serde_json::json!(["b"]);
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &value.to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // Sign for a different poll target.
    let wrong_target = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$other:hs",
        "vote-op-1",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &wrong_target,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    assert!(
        driver.poll_responses.lock().await.is_empty(),
        "invalid signatures must emit no Matrix response"
    );
    // The single-use challenge must not have been consumed by a failed
    // signature: a correctly signed request can still use it.
    let valid = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &valid,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn vote_against_ended_poll_is_rejected() {
    use cumments_core::models::PollEnd;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-ended",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    // An end from the poll creator's own virtual user authorizes the close.
    store
        .save_poll_end(&PollEnd {
            event_id: "$end:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@_cumments_test-blog_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:hs".to_string(),
            origin_server_ts: 100,
        })
        .await
        .expect("save end");
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[69u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(driver.poll_responses.lock().await.is_empty());
    assert!(store.lookup_operation("vote-op-1").await.unwrap().is_none());
}

#[tokio::test]
async fn vote_replay_after_poll_ended_succeeds_with_no_content() {
    use cumments_core::models::PollEnd;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-replay-ended",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[71u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-replay-ended",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );
    let post = || {
        router.clone().oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", "vote-op-replay-ended".to_string())],
            &body,
        ))
    };

    // 1. Initial vote while poll is open succeeds.
    let response = post().await.expect("first vote");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(driver.poll_responses.lock().await.len(), 1);

    // 2. Poll is ended by its creator.
    store
        .save_poll_end(&PollEnd {
            event_id: "$end:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@_cumments_test-blog_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:hs".to_string(),
            origin_server_ts: 200,
        })
        .await
        .expect("save end");

    let proj = store
        .get_poll_projection("$poll:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(proj.status, cumments_core::poll::PollStatus::Ended);

    // 3. Replay of the completed vote returns 204 No Content and emits no duplicate Matrix response.
    let replay_response = post().await.expect("replay vote");
    assert_eq!(replay_response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        driver.poll_responses.lock().await.len(),
        1,
        "replay must not emit a second Matrix response"
    );

    // 4. A new vote from a different author or key is rejected with 409 Conflict.
    let bob_key = SigningKey::from_bytes(&[72u8; 32]);
    let bob_challenge = state.pow.generate_challenge();
    let bob_challenge_response = solve_pow(&bob_challenge);
    let bob_body = signed_vote_body(
        &bob_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-bob-ended",
        &["b"],
        &bob_challenge.prefix,
        &bob_challenge_response,
    );
    let bob_response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", "vote-op-bob-ended".to_string())],
            &bob_body,
        ))
        .await
        .expect("bob vote");
    assert_eq!(bob_response.status(), StatusCode::CONFLICT);
    assert_eq!(driver.poll_responses.lock().await.len(), 1);
}

#[tokio::test]
async fn vote_invalid_pow_does_not_claim_the_operation() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-pow",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[70u8; 32]);
    let challenge = state.pow.generate_challenge();
    // Correct signature over the real challenge prefix, but an invalid PoW.
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-1",
        &["a"],
        &challenge.prefix,
        "not-a-valid-pow",
    );
    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &vote_key(),
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(driver.poll_responses.lock().await.is_empty());
    assert!(
        store.lookup_operation("vote-op-1").await.unwrap().is_none(),
        "invalid PoW must not claim the operation"
    );
}

#[tokio::test]
async fn vote_multiple_users_are_independent() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-multi-user",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());

    for (key_byte, option, op) in [(71u8, "a", "vote-op-user1"), (72u8, "b", "vote-op-user2")] {
        let signing_key = SigningKey::from_bytes(&[key_byte; 32]);
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let body = signed_vote_body(
            &signing_key,
            "test-blog",
            "hello",
            "$poll:hs",
            op,
            &[option],
            &challenge.prefix,
            &challenge_response,
        );
        let response = router
            .clone()
            .oneshot(request_with_body(
                Method::POST,
                vote_uri(),
                Some("null"),
                &[("idempotency-key", op.to_string())],
                &body,
            ))
            .await
            .expect("call router");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    assert_eq!(driver.poll_responses.lock().await.len(), 2);
}

#[tokio::test]
async fn vote_persists_txn_id_and_reuses_on_retry() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_core::submissions::OperationExecutionStatus;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-persist-txnid",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[80u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "vote-op-persist-1";
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );

    let post = || {
        router.clone().oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
    };

    // First Vote succeeds
    let resp1 = post().await.expect("call router");
    assert_eq!(resp1.status(), StatusCode::NO_CONTENT);

    // Verify claim exists and execution is Success
    let claim = store
        .lookup_operation(op_id)
        .await
        .unwrap()
        .expect("claim exists");
    assert_eq!(
        claim.author_public_key,
        URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes())
    );
    let execution = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution.status, OperationExecutionStatus::Success);
    assert!(!execution.txn_id.is_empty());

    let recorded1 = driver.poll_responses.lock().await.clone();
    assert_eq!(recorded1.len(), 1);
    assert_eq!(recorded1[0].txn_id, execution.txn_id);

    // Retry the same Vote
    let resp2 = post().await.expect("call router");
    assert_eq!(resp2.status(), StatusCode::NO_CONTENT);

    // Verify txn_id is unchanged and no second Matrix send occurred
    let execution2 = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution2.txn_id, execution.txn_id);
    assert_eq!(execution2.status, OperationExecutionStatus::Success);
    let recorded2 = driver.poll_responses.lock().await;
    assert_eq!(recorded2.len(), 1, "no second Matrix send on replay");
}

#[tokio::test]
async fn vote_transport_failure_retains_claim_and_reuses_txn_id() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_core::submissions::OperationExecutionStatus;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-transport-fail",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[81u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "vote-op-transport-fail-1";
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &["b"],
        &challenge.prefix,
        &challenge_response,
    );

    // Simulate transport error on the first attempt
    *driver.fail_poll_response_count.lock().await = 1;

    let post = || {
        router.clone().oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
    };

    let resp1 = post().await.expect("call router");
    assert_eq!(resp1.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // Verify: claim is NOT released! Execution state is Failed and txn_id is persisted
    let claim = store
        .lookup_operation(op_id)
        .await
        .unwrap()
        .expect("claim retained");
    assert_eq!(
        claim.author_public_key,
        URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes())
    );
    let execution1 = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution1.status, OperationExecutionStatus::Failed);
    let original_txn_id = execution1.txn_id.clone();
    assert!(!original_txn_id.is_empty());
    assert!(
        driver.poll_responses.lock().await.is_empty(),
        "first attempt failed before acceptance"
    );

    // Retry the exact same Vote
    let resp2 = post().await.expect("call router");
    assert_eq!(resp2.status(), StatusCode::NO_CONTENT);

    // Verify: execution is now Success, using the EXACT same txn_id, no second txn_id allocated
    let execution2 = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution2.status, OperationExecutionStatus::Success);
    assert_eq!(
        execution2.txn_id, original_txn_id,
        "retry must reuse the persisted txn_id"
    );

    let recorded = driver.poll_responses.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].txn_id, original_txn_id);
}

#[tokio::test]
async fn vote_ambiguous_lost_response_deduplicates_on_retry() {
    use cumments_core::submissions::OperationExecutionStatus;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-ambiguous-response",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[82u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "vote-op-ambiguous-1";
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );

    // Simulate: Matrix homeserver accepts the event, but the HTTP response to the caller is lost
    *driver.ambiguous_poll_response_count.lock().await = 1;

    let post = || {
        router.clone().oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
    };

    let resp1 = post().await.expect("call router");
    assert_eq!(resp1.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // Homeserver received the event with the persisted txn_id
    let recorded_after_first = driver.poll_responses.lock().await.clone();
    assert_eq!(recorded_after_first.len(), 1);
    let original_txn_id = recorded_after_first[0].txn_id.clone();

    let execution1 = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution1.status, OperationExecutionStatus::Failed);
    assert_eq!(execution1.txn_id, original_txn_id);

    // Retry the Vote
    let resp2 = post().await.expect("call router");
    assert_eq!(resp2.status(), StatusCode::NO_CONTENT);

    // Matrix deduplicated by txn_id: still exactly 1 event recorded
    let recorded_after_retry = driver.poll_responses.lock().await;
    assert_eq!(
        recorded_after_retry.len(),
        1,
        "Matrix deduplication prevents duplicate event"
    );
    assert_eq!(recorded_after_retry[0].txn_id, original_txn_id);

    let execution2 = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution2.status, OperationExecutionStatus::Success);
    assert_eq!(execution2.txn_id, original_txn_id);
}

#[tokio::test]
async fn vote_concurrent_identical_requests_single_txn_id_and_single_event() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_core::submissions::OperationExecutionStatus;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-concurrent-identical",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[83u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "vote-op-concurrent-1";
    let body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );

    let r1 = router.clone();
    let r2 = router.clone();
    let b1 = body.clone();
    let b2 = body.clone();

    let task1 = tokio::spawn(async move {
        r1.oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &b1,
        ))
        .await
        .expect("task 1")
    });

    let task2 = tokio::spawn(async move {
        r2.oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &b2,
        ))
        .await
        .expect("task 2")
    });

    let (resp1, resp2) = tokio::join!(task1, task2);
    assert_eq!(resp1.unwrap().status(), StatusCode::NO_CONTENT);
    assert_eq!(resp2.unwrap().status(), StatusCode::NO_CONTENT);

    // Verify: exactly ONE operation identity claimed
    let claim = store
        .lookup_operation(op_id)
        .await
        .unwrap()
        .expect("claim exists");
    assert_eq!(
        claim.author_public_key,
        URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes())
    );

    // Verify: exactly ONE execution record exists with status Success
    let execution = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution.status, OperationExecutionStatus::Success);

    // Verify: exactly ONE Matrix event was emitted with that exact txn_id
    let recorded = driver.poll_responses.lock().await;
    assert_eq!(recorded.len(), 1, "exactly one Matrix send occurred");
    assert_eq!(recorded[0].txn_id, execution.txn_id);
}

#[tokio::test]
async fn vote_retry_does_not_consume_or_require_fresh_pow() {
    use cumments_core::submissions::OperationExecutionStatus;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "vote-retry-no-pow",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[84u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "vote-op-retry-pow-1";

    let body_valid_pow = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );

    // Fail first attempt with transport failure
    *driver.fail_poll_response_count.lock().await = 1;

    let resp1 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body_valid_pow,
        ))
        .await
        .expect("call router");
    assert_eq!(resp1.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // Verify claim exists
    assert!(store.lookup_operation(op_id).await.unwrap().is_some());

    // On retry, provide an invalid PoW nonce; since the operation was already
    // claimed, PoW validation must be skipped entirely.
    let body_stale_pow = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &["a"],
        &challenge.prefix,
        &format!("{}|invalid_nonce", challenge.prefix),
    );

    let resp2 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body_stale_pow,
        ))
        .await
        .expect("call router");
    assert_eq!(
        resp2.status(),
        StatusCode::NO_CONTENT,
        "retry of already-claimed operation must not reject due to PoW"
    );

    let execution = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution.status, OperationExecutionStatus::Success);
    assert_eq!(driver.poll_responses.lock().await.len(), 1);
}

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cumments_core::poll::PollStatus;
use cumments_core::submissions::OperationExecutionStatus;

async fn seed_poll_with_creator(
    store: &DbStore,
    poll_id: &str,
    options: &[(&str, &str)],
    max_selections: u8,
    creator_public_key: &str,
) {
    use cumments_core::models::{PollContent, PollOption};
    let site = SiteId::from("test-blog");
    let slug = PageSlug::from("hello");
    let _ = store
        .register_site("test-blog", &token_hash("claim"), false)
        .await;
    let _ = store.register_room("!room:hs", &site, &slug).await;
    store
        .save_message(&Message {
            event_id: poll_id.to_string(),
            site_id: "test-blog".to_string(),
            page_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Visitor,
                display_name: Some("Alice".to_string()),
                avatar_url: None,
                media_reference: None,
                public_key: Some(creator_public_key.to_string()),
                mxid: None,
            },
            content: Content::Poll(PollContent {
                question: "q?".to_string(),
                answers: options
                    .iter()
                    .map(|(id, text)| PollOption {
                        id: id.to_string(),
                        text: text.to_string(),
                    })
                    .collect(),
                kind: cumments_core::poll::PollSemanticKind::Disclosed,
                max_selections: u64::from(max_selections),
                status: cumments_core::poll::PollStatus::Open,
                end_time: None,
                results: None,
                total_votes: 0,
                responses: Vec::new(),
                my_votes: None,
            }),
            matrix_event_type: "org.matrix.msc3381.poll.start".to_string(),
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
            sender_mxid: "@_cumments_test-blog_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:hs".to_string(),
            raw_content: serde_json::json!({}),
        })
        .await
        .expect("seed poll");
}

fn signed_end_poll_body(
    signing_key: &ed25519_dalek::SigningKey,
    site: &str,
    page: &str,
    poll_id: &str,
    operation_id: &str,
    challenge_prefix: &str,
    challenge_response: &str,
) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_core::poll::{end_poll_semantic_operation, poll_signature_envelope};
    use ed25519_dalek::Signer;

    let operation = end_poll_semantic_operation(site, page, poll_id);
    let envelope = poll_signature_envelope(&operation, operation_id, challenge_prefix);
    let signature = URL_SAFE_NO_PAD.encode(
        signing_key
            .sign(envelope.to_canonical_bytes().as_slice())
            .to_bytes(),
    );
    serde_json::json!({
        "author_public_key": URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string()
}

fn end_poll_uri() -> &'static str {
    "/api/v1/sites/test-blog/pages/hello/polls/$poll:hs/end"
}

#[tokio::test]
async fn end_poll_creator_emits_one_matrix_end() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-ok",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[71u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "end-op-1",
        &challenge.prefix,
        &challenge_response,
    );

    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", "end-op-1".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let recorded = driver.poll_ends.lock().await;
    assert_eq!(recorded.len(), 1, "exactly one poll end is emitted");
    assert_eq!(recorded[0].poll_event_id, "$poll:hs");
    assert_eq!(recorded[0].operation_id, "end-op-1");
    assert!(!recorded[0].txn_id.is_empty());
    drop(recorded);

    // End is synchronous and creates no durable submission.
    assert!(
        store
            .get_pending_post_submissions(10)
            .await
            .unwrap()
            .is_empty(),
        "End poll must not create a durable submission"
    );

    // Operation execution is Success in database.
    let execution = store
        .get_operation_execution("end-op-1")
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution.status, OperationExecutionStatus::Success);
}

#[tokio::test]
async fn end_poll_rejects_unauthorized_non_creator() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-auth",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[72u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    // Non-creator attempts to end poll
    let other_key = SigningKey::from_bytes(&[73u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let body = signed_end_poll_body(
        &other_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "end-op-other",
        &challenge.prefix,
        &challenge_response,
    );

    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", "end-op-other".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // No Matrix event was emitted
    assert!(driver.poll_ends.lock().await.is_empty());
    // No operation was claimed
    assert!(
        store
            .lookup_operation("end-op-other")
            .await
            .unwrap()
            .is_none()
    );
    // Authorization failure does not consume PoW
    assert!(
        state.pow.verify(&challenge_response),
        "authorization failure must not consume PoW"
    );
}

#[tokio::test]
async fn end_poll_signature_binds_target_operation_and_challenge() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-sig",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[74u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);

    // 1. Changed target poll_id in signature envelope
    let tampered_target_body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$other-poll:hs",
        "end-op-sig-1",
        &challenge.prefix,
        &challenge_response,
    );
    let res1 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", "end-op-sig-1".to_string())],
            &tampered_target_body,
        ))
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::FORBIDDEN);

    // 2. Changed operation_id in signature envelope
    let tampered_op_body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "different-op-id",
        &challenge.prefix,
        &challenge_response,
    );
    let res2 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", "end-op-sig-2".to_string())],
            &tampered_op_body,
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::FORBIDDEN);

    // 3. Changed challenge in signature envelope
    let tampered_chal_body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "end-op-sig-3",
        "wrong-prefix",
        &challenge_response,
    );
    let res3 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", "end-op-sig-3".to_string())],
            &tampered_chal_body,
        ))
        .await
        .unwrap();
    assert_eq!(res3.status(), StatusCode::FORBIDDEN);

    // No operations were claimed
    assert!(
        store
            .lookup_operation("end-op-sig-1")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .lookup_operation("end-op-sig-2")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .lookup_operation("end-op-sig-3")
            .await
            .unwrap()
            .is_none()
    );
    assert!(driver.poll_ends.lock().await.is_empty());
}

#[tokio::test]
async fn end_poll_replay_consumes_no_pow_and_emits_no_second_event() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-replay",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[75u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "end-op-replay-1";
    let body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &challenge.prefix,
        &challenge_response,
    );

    // First call -> 204
    let res1 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::NO_CONTENT);
    assert_eq!(driver.poll_ends.lock().await.len(), 1);

    // Replay with bogus challenge_response -> 204 (bypasses PoW verification)
    let replay_body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &challenge.prefix,
        &format!("{}|bogus_nonce", challenge.prefix),
    );
    let res2 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &replay_body,
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::NO_CONTENT);

    // No second Matrix event was emitted
    assert_eq!(driver.poll_ends.lock().await.len(), 1);
}

#[tokio::test]
async fn end_poll_conflicts_on_different_author_or_fingerprint() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-conflict",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[76u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    seed_poll_with_creator(
        &store,
        "$poll2:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "end-op-conflict-1";

    // Establish operation with creator_key for $poll:hs
    let body1 = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &challenge.prefix,
        &challenge_response,
    );
    let res1 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body1,
        ))
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::NO_CONTENT);

    // 1. Same op_id with DIFFERENT author -> 409 Conflict
    let other_key = SigningKey::from_bytes(&[77u8; 32]);
    let chal2 = state.pow.generate_challenge();
    let chal2_resp = solve_pow(&chal2);
    let body_diff_author = signed_end_poll_body(
        &other_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &chal2.prefix,
        &chal2_resp,
    );
    let res_author = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body_diff_author,
        ))
        .await
        .unwrap();
    assert_eq!(res_author.status(), StatusCode::CONFLICT);

    // 2. Same op_id with DIFFERENT target ($poll2:hs) -> 409 Conflict
    let chal3 = state.pow.generate_challenge();
    let chal3_resp = solve_pow(&chal3);
    let body_diff_target = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll2:hs",
        op_id,
        &chal3.prefix,
        &chal3_resp,
    );
    let res_target = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls/$poll2:hs/end",
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body_diff_target,
        ))
        .await
        .unwrap();
    assert_eq!(res_target.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn end_poll_persists_txn_id_and_reuses_on_retry() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-retry-tx",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[78u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "end-op-persists-txn";
    let body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &challenge.prefix,
        &challenge_response,
    );

    let res = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let recorded = driver.poll_ends.lock().await;
    assert_eq!(recorded.len(), 1);
    let first_txn = recorded[0].txn_id.clone();
    drop(recorded);

    let execution = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(execution.txn_id, first_txn);
    assert_eq!(execution.status, OperationExecutionStatus::Success);
}

#[tokio::test]
async fn end_poll_transport_failure_retains_claim_and_reuses_txn_id() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-trans-fail",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[79u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    // Inject transport failure for the first Matrix send
    *driver.fail_poll_end_count.lock().await = 1;

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "end-op-fail-retry";
    let body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &challenge.prefix,
        &challenge_response,
    );

    // First attempt fails with 500
    let res1 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // Claim must NOT be released!
    let claim = store
        .lookup_operation(op_id)
        .await
        .unwrap()
        .expect("claim retained");
    assert_eq!(claim.author_public_key, creator_pk);

    // Execution record remains in Failed status with persisted txn_id
    let exec1 = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(exec1.status, OperationExecutionStatus::Failed);
    let original_txn = exec1.txn_id.clone();

    // Retry the exact same End Poll operation
    let res2 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::NO_CONTENT);

    // Verify the retry reused the original txn_id
    let recorded = driver.poll_ends.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].txn_id, original_txn);
    drop(recorded);

    let exec2 = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(exec2.txn_id, original_txn);
    assert_eq!(exec2.status, OperationExecutionStatus::Success);
}

#[tokio::test]
async fn end_poll_ambiguous_lost_response_deduplicates_on_retry() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-ambig",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[80u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    // Simulate homeserver accepts event, but response is lost
    *driver.ambiguous_poll_end_count.lock().await = 1;

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "end-op-ambig";
    let body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &challenge.prefix,
        &challenge_response,
    );

    // First attempt returns error
    let res1 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // Event was recorded by the driver
    assert_eq!(driver.poll_ends.lock().await.len(), 1);

    // Client retries
    let res2 = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::NO_CONTENT);

    // Matrix deduplicated by (room_id, txn_id) -> still exactly 1 event!
    assert_eq!(driver.poll_ends.lock().await.len(), 1);
}

#[tokio::test]
async fn end_poll_concurrent_identical_requests_single_txn_id_and_single_event() {
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-concurrent",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[81u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "end-op-concurrent";
    let body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &challenge.prefix,
        &challenge_response,
    );

    let r1 = router.clone();
    let r2 = router.clone();
    let b1 = body.clone();
    let b2 = body.clone();

    let task1 = tokio::spawn(async move {
        r1.oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &b1,
        ))
        .await
        .unwrap()
    });

    let task2 = tokio::spawn(async move {
        r2.oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &b2,
        ))
        .await
        .unwrap()
    });

    let (res1, res2) = tokio::join!(task1, task2);
    assert_eq!(res1.unwrap().status(), StatusCode::NO_CONTENT);
    assert_eq!(res2.unwrap().status(), StatusCode::NO_CONTENT);

    // Exactly one Matrix event emitted
    assert_eq!(driver.poll_ends.lock().await.len(), 1);

    // Exactly one execution record established
    let exec = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("execution exists");
    assert_eq!(exec.status, OperationExecutionStatus::Success);
}

#[tokio::test]
async fn end_poll_against_already_ended_poll_is_rejected() {
    use cumments_core::models::PollEnd;
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;

    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "end-poll-ended",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;

    let creator_key = SigningKey::from_bytes(&[82u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    // Projector records a valid PollEnd from creator
    store
        .save_poll_end(&PollEnd {
            event_id: "$end-ev:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: "@_cumments_test-blog_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:hs".to_string(),
            origin_server_ts: 100,
        })
        .await
        .unwrap();

    let projection = store
        .get_poll_projection("$poll:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(projection.status, PollStatus::Ended);

    // A NEW logical End operation against the ended poll is rejected with 409 Conflict
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "end-op-new-against-ended";
    let body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll:hs",
        op_id,
        &challenge.prefix,
        &challenge_response,
    );

    let response = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            end_poll_uri(),
            Some("null"),
            &[("idempotency-key", op_id.to_string())],
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // No Matrix event was emitted
    assert!(driver.poll_ends.lock().await.is_empty());
}

#[tokio::test]
async fn end_poll_reducer_first_valid_end_wins() {
    use cumments_core::models::PollEnd;
    use ed25519_dalek::SigningKey;

    let (_state, store) =
        test_state("end-poll-reducer", SiteVerificationPolicy::Disabled, None).await;

    let creator_key = SigningKey::from_bytes(&[83u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;

    // Initially open
    let p0 = store
        .get_poll_projection("$poll:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p0.status, PollStatus::Open);
    assert!(p0.end.is_none());

    let sender = "@_cumments_test-blog_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:hs";

    // First valid end at ts = 100
    store
        .save_poll_end(&PollEnd {
            event_id: "$end-first:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: sender.to_string(),
            origin_server_ts: 100,
        })
        .await
        .unwrap();

    let p1 = store
        .get_poll_projection("$poll:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p1.status, PollStatus::Ended);
    assert_eq!(p1.end.as_ref().map(|e| e.origin_server_ts), Some(100));

    // Later end at ts = 200 (first valid end wins; later end does not change ended_at)
    store
        .save_poll_end(&PollEnd {
            event_id: "$end-later:hs".to_string(),
            poll_message_id: "$poll:hs".to_string(),
            sender_mxid: sender.to_string(),
            origin_server_ts: 200,
        })
        .await
        .unwrap();

    let p2 = store
        .get_poll_projection("$poll:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p2.status, PollStatus::Ended);
    assert_eq!(p2.end.as_ref().map(|e| e.origin_server_ts), Some(100));
    assert_eq!(
        p2.end.as_ref().map(|e| e.event_id.as_str()),
        Some("$end-first:hs")
    );
}

// ── Poll Read-Side Integration & Realtime Tests ───────────────────

fn sign_query_comments(
    signing_key: &ed25519_dalek::SigningKey,
    site: &str,
    page: &str,
) -> (String, String) {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_core::identity::signature_message;
    use ed25519_dalek::Signer;

    let pk = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let msg = signature_message(&[Some("QUERY_COMMENTS"), Some(site), Some(page)]);
    let sig = URL_SAFE_NO_PAD.encode(signing_key.sign(msg.as_bytes()).to_bytes());
    (pk, sig)
}

#[tokio::test]
async fn poll_read_disclosed_open_undisclosed_open_and_ended_visibility() {
    use cumments_core::models::{PollContent, PollEnd, PollOption, PollVote};
    use cumments_core::poll::{PollSemanticKind, PollStatus};
    let (state, store) = test_state(
        "poll-read-visibility",
        SiteVerificationPolicy::Disabled,
        None,
    )
    .await;
    let router = cumments_api::build_router(state.clone());

    // 1. Seed disclosed poll ($poll-disc:hs) with options "opt-a" and "opt-b"
    seed_poll(
        &store,
        "$poll-disc:hs",
        &[("opt-a", "Option A"), ("opt-b", "Option B")],
        1,
    )
    .await;

    // 2. Seed undisclosed poll ($poll-undisc:hs) with options "opt-x" and "opt-y"
    store
        .save_message(&Message {
            event_id: "$poll-undisc:hs".to_string(),
            site_id: "test-blog".to_string(),
            page_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Visitor,
                display_name: Some("Alice".to_string()),
                avatar_url: None,
                media_reference: None,
                public_key: Some("creator-key".to_string()),
                mxid: None,
            },
            content: Content::Poll(PollContent {
                question: "Secret question?".to_string(),
                answers: vec![
                    PollOption {
                        id: "opt-x".to_string(),
                        text: "Option X".to_string(),
                    },
                    PollOption {
                        id: "opt-y".to_string(),
                        text: "Option Y".to_string(),
                    },
                ],
                kind: PollSemanticKind::Undisclosed,
                max_selections: 1,
                status: PollStatus::Open,
                end_time: None,
                results: None,
                total_votes: 0,
                responses: Vec::new(),
                my_votes: None,
            }),
            matrix_event_type: "org.matrix.msc3381.poll.start".to_string(),
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
            sender_mxid: "@_cumments_test-blog_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:hs".to_string(),
            raw_content: serde_json::Value::Null,
        })
        .await
        .unwrap();

    // Alice votes for "opt-a" on disclosed poll
    store
        .save_poll_vote_with_selections(
            &PollVote {
                event_id: "$v1:hs".to_string(),
                poll_message_id: "$poll-disc:hs".to_string(),
                sender_mxid: "@alice:hs".to_string(),
                option_index: None,
                origin_server_ts: 100,
            },
            &["opt-a".to_string()],
            None,
        )
        .await
        .unwrap();

    // Bob votes for "opt-x" on undisclosed poll
    store
        .save_poll_vote_with_selections(
            &PollVote {
                event_id: "$v2:hs".to_string(),
                poll_message_id: "$poll-undisc:hs".to_string(),
                sender_mxid: "@bob:hs".to_string(),
                option_index: None,
                origin_server_ts: 100,
            },
            &["opt-x".to_string()],
            None,
        )
        .await
        .unwrap();

    // Query comments collection
    let res = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
            "",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let comments: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let items = comments["data"].as_array().unwrap();
    assert_eq!(items.len(), 2);

    let disc_item = items
        .iter()
        .find(|i| i["event_id"] == "$poll-disc:hs")
        .unwrap();
    let disc_poll = &disc_item["content"];
    assert_eq!(disc_poll["kind"], "disclosed");
    assert_eq!(disc_poll["status"], "open");
    assert!(disc_poll["end_time"].is_null());
    assert_eq!(disc_poll["total_votes"], 1);
    // Declared answers order preserved
    assert_eq!(disc_poll["answers"][0]["id"], "opt-a");
    assert_eq!(disc_poll["answers"][1]["id"], "opt-b");
    // Results exposed, all declared answers mapped to integer counts, 0 included
    assert_eq!(disc_poll["results"]["opt-a"], 1);
    assert_eq!(disc_poll["results"]["opt-b"], 0);

    let undisc_item = items
        .iter()
        .find(|i| i["event_id"] == "$poll-undisc:hs")
        .unwrap();
    let undisc_poll = &undisc_item["content"];
    assert_eq!(undisc_poll["kind"], "undisclosed");
    assert_eq!(undisc_poll["status"], "open");
    assert!(undisc_poll["end_time"].is_null());
    assert_eq!(undisc_poll["total_votes"], 1);
    // Undisclosed while open: results MUST be null
    assert!(
        undisc_poll["results"].is_null(),
        "undisclosed open poll results must be null"
    );

    // Now end the undisclosed poll
    store
        .save_poll_end(&PollEnd {
            event_id: "$end-undisc:hs".to_string(),
            poll_message_id: "$poll-undisc:hs".to_string(),
            sender_mxid: "@_cumments_test-blog_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:hs".to_string(),
            origin_server_ts: 500,
        })
        .await
        .unwrap();

    // Query comments collection again
    let res2 = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
            "",
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    let body_bytes2 = axum::body::to_bytes(res2.into_body(), usize::MAX)
        .await
        .unwrap();
    let comments2: serde_json::Value = serde_json::from_slice(&body_bytes2).unwrap();
    let items2 = comments2["data"].as_array().unwrap();
    let undisc_ended = items2
        .iter()
        .find(|i| i["event_id"] == "$poll-undisc:hs")
        .unwrap();
    let ended_poll = &undisc_ended["content"];
    assert_eq!(ended_poll["kind"], "undisclosed");
    assert_eq!(ended_poll["status"], "ended");
    assert_eq!(ended_poll["end_time"], 500);
    assert_eq!(ended_poll["total_votes"], 1);
    // Undisclosed ended poll: results MUST now be exposed and unmasked
    assert_eq!(ended_poll["results"]["opt-x"], 1);
    assert_eq!(ended_poll["results"]["opt-y"], 0);
}

#[tokio::test]
async fn poll_read_single_comment_lookup_and_my_votes_personalization() {
    use cumments_core::models::PollVote;
    use ed25519_dalek::SigningKey;
    let (state, store) =
        test_state("poll-my-votes-read", SiteVerificationPolicy::Disabled, None).await;
    let router = cumments_api::build_router(state.clone());

    seed_poll(&store, "$poll:hs", &[("opt0", "Zero"), ("opt1", "One")], 1).await;

    let alice_key = SigningKey::from_bytes(&[81u8; 32]);
    let bob_key = SigningKey::from_bytes(&[82u8; 32]);
    let (alice_pk, alice_sig) = sign_query_comments(&alice_key, "test-blog", "hello");
    let (bob_pk, bob_sig) = sign_query_comments(&bob_key, "test-blog", "hello");

    // Alice voter mxid derived from public key
    let alice_visitor_id = derive_visitor_id_from_public_key(&alice_pk).unwrap();
    let alice_mxid = format!("@_cumments_test-blog_{}:hs", alice_visitor_id);

    // Alice votes for "opt0"
    store
        .save_poll_vote_with_selections(
            &PollVote {
                event_id: "$v-alice:hs".to_string(),
                poll_message_id: "$poll:hs".to_string(),
                sender_mxid: alice_mxid,
                option_index: None,
                origin_server_ts: 100,
            },
            &["opt0".to_string()],
            None,
        )
        .await
        .unwrap();

    // 1. Single comment lookup (GET .../comments/{comment_id})
    // Unauthenticated GET: my_votes is null
    let res_single = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/sites/test-blog/pages/hello/comments/$poll%3Ahs",
            Some("null"),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(res_single.status(), StatusCode::OK);
    let single_bytes = axum::body::to_bytes(res_single.into_body(), usize::MAX)
        .await
        .unwrap();
    let single_json: serde_json::Value = serde_json::from_slice(&single_bytes).unwrap();
    assert_eq!(single_json["event_id"], "$poll:hs");
    assert!(
        single_json["content"]["my_votes"].is_null(),
        "unauthenticated single lookup has my_votes: null"
    );
    assert_eq!(single_json["content"]["results"]["opt0"], 1);
    assert_eq!(single_json["content"]["results"]["opt1"], 0);

    // 2. Collection query: Unauthenticated (empty query body)
    let res_anon = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
            "",
        ))
        .await
        .unwrap();
    assert_eq!(res_anon.status(), StatusCode::OK);
    let anon_bytes = axum::body::to_bytes(res_anon.into_body(), usize::MAX)
        .await
        .unwrap();
    let anon_json: serde_json::Value = serde_json::from_slice(&anon_bytes).unwrap();
    let anon_poll = &anon_json["data"][0]["content"];
    assert!(
        anon_poll["my_votes"].is_null(),
        "unauthenticated collection query has my_votes: null"
    );

    // 3. Collection query: Authenticated as Alice (voted for "opt0")
    let alice_body = serde_json::json!({
        "author_public_key": alice_pk,
        "author_signature": alice_sig,
    })
    .to_string();
    let res_alice = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
            &alice_body,
        ))
        .await
        .unwrap();
    assert_eq!(res_alice.status(), StatusCode::OK);
    let alice_bytes = axum::body::to_bytes(res_alice.into_body(), usize::MAX)
        .await
        .unwrap();
    let alice_json: serde_json::Value = serde_json::from_slice(&alice_bytes).unwrap();
    let alice_poll = &alice_json["data"][0]["content"];
    assert_eq!(
        alice_poll["my_votes"],
        serde_json::json!(["opt0"]),
        "Alice receives her vote selection"
    );

    // 4. Collection query: Authenticated as Bob (has not voted)
    let bob_body = serde_json::json!({
        "author_public_key": bob_pk,
        "author_signature": bob_sig,
    })
    .to_string();
    let res_bob = router
        .clone()
        .oneshot(request_with_body(
            query_method(),
            "/api/v1/sites/test-blog/pages/hello/comments",
            Some("null"),
            &[],
            &bob_body,
        ))
        .await
        .unwrap();
    assert_eq!(res_bob.status(), StatusCode::OK);
    let bob_bytes = axum::body::to_bytes(res_bob.into_body(), usize::MAX)
        .await
        .unwrap();
    let bob_json: serde_json::Value = serde_json::from_slice(&bob_bytes).unwrap();
    let bob_poll = &bob_json["data"][0]["content"];
    assert_eq!(
        bob_poll["my_votes"],
        serde_json::json!([]),
        "Authenticated non-voter receives empty array"
    );
}

#[tokio::test]
async fn poll_mutations_do_not_emit_sse_directly() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use cumments_test_utils::TestDriver;
    use ed25519_dalek::SigningKey;
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver(
        "poll-mutation-no-sse",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    seed_poll(&store, "$poll:hs", &[("a", "A"), ("b", "B")], 1).await;
    let router = cumments_api::build_router(state.clone());

    // Subscribe to event_bus before any HTTP requests
    let mut rx = state.event_bus.subscribe();

    // 1. Post a vote via HTTP API
    let signing_key = SigningKey::from_bytes(&[61u8; 32]);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let vote_body = signed_vote_body(
        &signing_key,
        "test-blog",
        "hello",
        "$poll:hs",
        "vote-op-no-sse",
        &["a"],
        &challenge.prefix,
        &challenge_response,
    );

    let res_vote = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            vote_uri(),
            Some("null"),
            &[("idempotency-key", "vote-op-no-sse".to_string())],
            &vote_body,
        ))
        .await
        .unwrap();
    assert_eq!(res_vote.status(), StatusCode::NO_CONTENT);

    // Event bus MUST NOT have received any SSE event from HTTP vote handler
    assert!(
        rx.try_recv().is_err(),
        "HTTP vote mutation handler must not emit SSE directly"
    );

    // 2. End poll via HTTP API
    let creator_key = SigningKey::from_bytes(&[88u8; 32]);
    let creator_pk = URL_SAFE_NO_PAD.encode(creator_key.verifying_key().to_bytes());
    seed_poll_with_creator(
        &store,
        "$poll2:hs",
        &[("a", "A"), ("b", "B")],
        1,
        &creator_pk,
    )
    .await;
    let end_challenge = state.pow.generate_challenge();
    let end_challenge_response = solve_pow(&end_challenge);
    let end_body = signed_end_poll_body(
        &creator_key,
        "test-blog",
        "hello",
        "$poll2:hs",
        "end-op-no-sse",
        &end_challenge.prefix,
        &end_challenge_response,
    );

    let res_end = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/pages/hello/polls/$poll2:hs/end",
            Some("null"),
            &[("idempotency-key", "end-op-no-sse".to_string())],
            &end_body,
        ))
        .await
        .unwrap();
    assert_eq!(res_end.status(), StatusCode::NO_CONTENT);

    // Event bus MUST NOT have received any SSE event from HTTP end handler
    assert!(
        rx.try_recv().is_err(),
        "HTTP end mutation handler must not emit SSE directly"
    );
}

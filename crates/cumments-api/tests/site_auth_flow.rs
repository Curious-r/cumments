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
use cumments_core::governance::{NewRoleClaim, OWNER_LEVEL, RoleEntry};
use cumments_core::identity::{post_signature_message, signature_message};
use cumments_core::models::{PostSlug, SiteId};
use cumments_core::ports::{
    GovernanceStore, MessageStore, RegistryStore, RoleClaimStore, SiteAuthStore, SiteStore,
    StickerPackStore,
};
use cumments_core::site_auth::{
    Origin, SiteAuthPolicy, SiteVerificationPolicy, sha256_hex, site_request_signature, token_hash,
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
        registration_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        verification_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        operator_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(60))),
        confirm_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        trusted_proxies: Arc::new(Default::default()),
        // The existing integration test verifies against 127.0.0.1.
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
        moderation_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        ephemeral_bus: tokio::sync::broadcast::channel(16).0,
        ephemeral_state: None,
    };
    (state, store)
}

fn middleware_router(state: ApiState) -> Router {
    async fn ok_handler() -> StatusCode {
        StatusCode::OK
    }
    Router::new()
        .route(
            "/api/v1/sites/{site_id}/posts/{post_slug}/comments",
            post(ok_handler).fallback(ok_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/posts/{post_slug}/comments/{comment_id}",
            post(ok_handler).patch(ok_handler),
        )
        .route(
            "/api/v1/sites/{site_id}/posts/{post_slug}/media",
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

fn request_raw(
    method: Method,
    uri: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/octet-stream");
    for (name, value) in headers {
        builder = builder.header(*name, value.as_str());
    }
    let mut req = builder.body(Body::from(body)).expect("build request");
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
            "/api/v1/sites/test-blog/posts/hello/comments",
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
            "/api/v1/sites/test-blog/posts/hello/comments",
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
            "/api/v1/sites/test-blog/posts/hello/comments",
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
            "/api/v1/sites/test-blog/posts/hello/comments",
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
            "/api/v1/sites/test-blog/posts/hello/comments",
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
            "/api/v1/sites/Bad-Site/posts/hello/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(invalid_site.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response_origin(&invalid_site).as_deref(), Some("*"));

    // In `disabled` mode the middleware deliberately does not buffer write
    // bodies (guest media uploads keep the handler's 20MB cap instead of a
    // 1MB middleware cap), so a large body passes through to the handler and
    // the handler response still carries wildcard CORS.
    let oversized = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sites/test-blog/posts/hello/comments")
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
async fn comment_body_endpoints_require_comment_id() {
    let (state, store) = test_state("comment-body", SiteVerificationPolicy::Disabled, None).await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state);

    let delete = router
        .clone()
        .oneshot(
            request(
                Method::DELETE,
                "/api/v1/sites/test-blog/posts/hello/comments",
                Some("null"),
                &[],
            )
            .map(|_| {
                Body::from(
                    serde_json::json!({
                        "author_public_key": "pk",
                        "author_signature": "sig",
                        "challenge_response": "chal|nonce",
                    })
                    .to_string(),
                )
            }),
        )
        .await
        .expect("call router");
    assert_eq!(delete.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(delete)
            .await
            .contains("comment_id query parameter is required")
    );

    let patch = router
        .clone()
        .oneshot(
            request(
                Method::PATCH,
                "/api/v1/sites/test-blog/posts/hello/comments",
                Some("null"),
                &[],
            )
            .map(|_| {
                Body::from(
                    serde_json::json!({
                        "content": "edited",
                        "author_public_key": "pk",
                        "author_signature": "sig",
                        "challenge_response": "chal|nonce",
                    })
                    .to_string(),
                )
            }),
        )
        .await
        .expect("call router");
    assert_eq!(patch.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(patch).await.contains("comment_id is required"));
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
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/posts/hello/comments",
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
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/posts/hello/comments",
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
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/posts/hello/comments",
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
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/posts/hello/comments",
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
    let uri = format!("/api/v1/sites/{site_id}/posts/hello/comments");

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
    let media_uri = format!("/api/v1/sites/{site_id}/posts/hello/media");
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
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/posts/hello/comments",
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
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/posts/hello/comments",
            None,
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(response_origin(&read).as_deref(), Some("*"));
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
                "/api/v1/sites/test-blog/me/avatar",
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
            "/api/v1/sites/test-blog/me/avatar",
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
            "/api/v1/sites/test-blog/me/avatar",
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
            "/api/v1/sites/test-blog/me/avatar",
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
            "/api/v1/sites/not-registered/me/avatar",
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
            &format!("/api/v1/operator/sites/{site_id}/secret/rotate"),
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
                &format!("/api/v1/operator/sites/{site_id}/origins/revoke"),
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
            &format!("/api/v1/operator/sites/{site_id}/claim-token/rotate"),
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
    let slug = PostSlug::from("hello");
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
            "/api/v1/operator/rooms/quarantined",
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
            "/api/v1/operator/rooms/quarantined",
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
            "/api/v1/operator/rooms/quarantined/!room:hs",
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
            "/api/v1/operator/rooms/quarantined/!room:hs",
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
            "/api/v1/operator/rooms/quarantined/!unknown:hs",
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
            "/api/v1/operator/rooms/quarantined",
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
    let slug = PostSlug::from("hello");
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
    let display_name = "Alice";
    let message = signature_message(&[
        "LOCATE",
        "test-blog",
        "hello",
        "geo:31.2,121.5",
        &challenge.prefix,
    ]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "geo_uri": "geo:31.2,121.5",
        "description": "here",
        "display_name": display_name,
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string();

    let post = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/posts/hello/location",
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
            "/api/v1/sites/test-blog/posts/hello/location",
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
    let display_name = "Alice";
    let message =
        post_signature_message("test-blog", "hello", "hello world", None, &challenge.prefix);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "content": "hello world",
        "display_name": display_name,
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    })
    .to_string();

    let post = || {
        router.clone().oneshot(request_with_body(
            Method::POST,
            "/api/v1/sites/test-blog/posts/hello/comments",
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
    let display_name = "Alice";
    let media_url = "mxc://hs/cat";
    let message = post_signature_message("test-blog", "hello", media_url, None, &challenge.prefix);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "content": "",
        "media": {
            "url": media_url,
            "filename": "cat.png",
            "mimetype": "image/png",
        },
        "display_name": display_name,
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
            "/api/v1/sites/test-blog/posts/hello/comments",
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
            "/api/v1/sites/test-blog/posts/hello/comments",
            Some("null"),
            &[("idempotency-key", "media-key-123456".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn guest_avatar_set_and_delete_are_signed_and_idempotent() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    let driver = Arc::new(cumments_test_utils::TestDriver::new());
    let (state, store) = test_state_with_driver(
        "avatar",
        SiteVerificationPolicy::Disabled,
        None,
        driver.clone(),
    )
    .await;
    store
        .register_site("test-blog", &token_hash("claim"), false)
        .await
        .expect("register site");
    let router = cumments_api::build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[17u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let avatar_body = b"fake-png-bytes".to_vec();

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let message = signature_message(&[
        "UPLOAD_AVATAR",
        "test-blog",
        "image/png",
        &sha256_hex(&avatar_body),
        &challenge.prefix,
    ]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let uri = format!(
        "/api/v1/sites/test-blog/me/avatar?author_public_key={public_key}&author_signature={signature}&challenge_response={challenge_response}&mime=image%2Fpng&filename=avatar.png"
    );

    let put = router
        .clone()
        .oneshot(request_raw(
            Method::PUT,
            &uri,
            &[("idempotency-key", "avatar-key-123456".to_string())],
            avatar_body.clone(),
        ))
        .await
        .expect("call router");
    assert_eq!(put.status(), StatusCode::OK);
    assert!(put.headers().get("idempotent-replayed").is_none());
    let put_json: serde_json::Value = serde_json::from_str(&body_text(put).await).expect("parse");
    let avatar_url = put_json["avatar_url"]
        .as_str()
        .expect("avatar url")
        .to_string();
    assert!(avatar_url.starts_with("mxc://hs/test-blog/"));
    assert_eq!(
        *driver.avatar_updates.lock().await,
        vec![(
            public_key.clone(),
            "test-blog".to_string(),
            Some(avatar_url.clone())
        )]
    );

    // Replaying the same idempotency key returns the original URL and marks
    // the replay without consuming fresh PoW.
    let replay = router
        .clone()
        .oneshot(request_raw(
            Method::PUT,
            &uri,
            &[("idempotency-key", "avatar-key-123456".to_string())],
            avatar_body.clone(),
        ))
        .await
        .expect("call router");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(
        replay
            .headers()
            .get("idempotent-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let replay_json: serde_json::Value =
        serde_json::from_str(&body_text(replay).await).expect("parse");
    assert_eq!(replay_json["avatar_url"], avatar_url);

    // Non-image uploads are rejected before any Matrix write.
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let bad_body = b"not-an-image".to_vec();
    let message = signature_message(&[
        "UPLOAD_AVATAR",
        "test-blog",
        "video/mp4",
        &sha256_hex(&bad_body),
        &challenge.prefix,
    ]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let bad_uri = format!(
        "/api/v1/sites/test-blog/me/avatar?author_public_key={public_key}&author_signature={signature}&challenge_response={challenge_response}&mime=video%2Fmp4&filename=clip.mp4"
    );
    let denied = router
        .clone()
        .oneshot(request_raw(
            Method::PUT,
            &bad_uri,
            &[("idempotency-key", "avatar-key-654321".to_string())],
            bad_body,
        ))
        .await
        .expect("call router");
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(denied).await.contains("must be an image"));

    // Delete removes the profile field with its own signature.
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let message = signature_message(&["DELETE_AVATAR", "test-blog", &challenge.prefix]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let delete_uri = format!(
        "/api/v1/sites/test-blog/me/avatar?author_public_key={public_key}&author_signature={signature}&challenge_response={challenge_response}"
    );
    let deleted = router
        .clone()
        .oneshot(request_raw(Method::DELETE, &delete_uri, &[], Vec::new()))
        .await
        .expect("call router");
    assert_eq!(deleted.status(), StatusCode::OK);
    let updates = driver.avatar_updates.lock().await.clone();
    assert_eq!(
        updates.last(),
        Some(&(public_key, "test-blog".to_string(), None))
    );
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

    let owner_uri = format!("/api/v1/sites/{site_id}/owners");
    let owner_body = serde_json::json!({ "user_id": "@owner:hs" }).to_string();

    // Missing claim token is rejected before any Matrix write.
    let denied = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            &owner_uri,
            None,
            &[],
            &owner_body,
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
            &owner_uri,
            None,
            &[("x-cumments-claim-token", "claim-token".to_string())],
            &owner_body,
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
                &owner_uri,
                None,
                &[("x-cumments-claim-token", "claim-token".to_string())],
                &serde_json::json!({ "user_id": raw }).to_string(),
            ))
            .await
            .expect("call router");
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST, "{label}");
    }

    // The operator mirror works without a claim token.
    let co_uri = format!("/api/v1/operator/sites/{site_id}/co-managers");
    let added_co = router
        .clone()
        .oneshot(request_with_body(
            Method::POST,
            &co_uri,
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
            &serde_json::json!({ "user_id": "@co:hs" }).to_string(),
        ))
        .await
        .expect("call router");
    assert_eq!(added_co.status(), StatusCode::OK);
    let co_json: serde_json::Value =
        serde_json::from_str(&body_text(added_co).await).expect("parse response");
    assert_eq!(co_json["pending"], serde_json::json!(true));
    assert_eq!(co_json["level"], 75);

    // Deleting a pending claim revokes it without touching Matrix.
    let removed = router
        .clone()
        .oneshot(request_with_body(
            Method::DELETE,
            &format!("{owner_uri}?user_id=%40owner%3Ahs"),
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
                    user_id: "@co:hs".into(),
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
    assert_eq!(listed_json["owners"], serde_json::json!(["@owner:hs"]));
    assert_eq!(listed_json["co_managers"], serde_json::json!(["@co:hs"]));
}

#[tokio::test]
async fn applied_owner_revocation_marks_the_claim_revoked() {
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
            level: OWNER_LEVEL,
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
                level: OWNER_LEVEL,
            }],
        )
        .await
        .expect("project owner");

    let router = cumments_api::build_router(state);
    let removed = router
        .clone()
        .oneshot(request_with_body(
            Method::DELETE,
            &format!("/api/v1/sites/{site_id}/owners?user_id=%40owner%3Ahs"),
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
            "/api/v1/sites/reg-site/posts/p1/comments",
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
            "/api/v1/sites/ghost-site/posts/p1/comments",
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
            "/api/v1/sites/custom-blog/posts/p1/comments",
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
            "/api/v1/sites/a1b2c3d4e5f60718a1b2c3d4e5f60718/posts/p1/comments",
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
            "/api/v1/sites/custom-blog/posts/p1/comments",
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
            "/api/v1/sites/orphan-blog/posts/p1/comments",
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
            Method::DELETE,
            "/api/v1/sites/retire-blog",
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
            Method::DELETE,
            "/api/v1/sites/retire-blog",
            None,
            &[("x-cumments-claim-token", "claim".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(retired.status(), StatusCode::OK);
    let retired_json: serde_json::Value =
        serde_json::from_str(&body_text(retired).await).expect("parse response");
    assert_eq!(retired_json["status"], "retiring");

    // Writes now fail with 410 site-retired, even in the disabled policy.
    let write = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/sites/retire-blog/posts/p1/comments",
            Some("null"),
            &[],
        ))
        .await
        .expect("call router");
    assert_eq!(write.status(), StatusCode::GONE);
    assert!(body_text(write).await.contains("site-retired"));

    // The claim token was cleared, so a second owner attempt is unauthenticated.
    let again = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            "/api/v1/sites/retire-blog",
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
            Method::DELETE,
            "/api/v1/operator/sites/operator-retire",
            None,
            &[("authorization", "Bearer test-operator-token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(operator.status(), StatusCode::OK);
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
            "/api/v1/sites/test-blog/packs/default/stickers?shortcode=cat",
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
    let message = post_signature_message("test-blog", "hello", media_url, None, &challenge.prefix);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let body = serde_json::json!({
        "content": "",
        "media": {
            "url": media_url,
            "kind": "sticker",
        },
        "display_name": "Alice",
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
            "/api/v1/sites/test-blog/posts/hello/comments",
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
            "/api/v1/sites/test-blog/posts/hello/comments",
            Some("null"),
            &[("idempotency-key", "sticker-key-123456".to_string())],
            &body,
        ))
        .await
        .expect("call router");
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
}

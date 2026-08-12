//! Router-level integration tests for write-path site authentication, the
//! admin API, and the well-known verification flow.

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
    middleware,
    routing::{get, post},
};
use cumments_api::{ApiState, pow::Pow, rate_limit::RateLimiter, site_auth::enforce_site_auth};
use cumments_core::models::{PostSlug, SiteId};
use cumments_core::ports::{RegistryStore, SiteAuthStore};
use cumments_core::site_auth::{
    Origin, SiteAuthPolicy, SiteVerificationPolicy, site_request_signature, token_hash,
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
    admin_token: Option<&str>,
) -> (ApiState, DbStore) {
    let store = DbStore::connect(&test_db_url(name))
        .await
        .expect("connect test database");
    let (event_bus, _) = tokio::sync::broadcast::channel(100);
    let state = ApiState {
        store: Arc::new(store.clone()),
        pow: Arc::new(Pow::new("test-secret".to_string(), 1)),
        event_bus,
        reconciler_notify: Arc::new(tokio::sync::Notify::new()),
        site_auth_policy: Arc::new(SiteAuthPolicy {
            verification: policy,
            sites: Default::default(),
        }),
        admin_token_hash: admin_token.map(token_hash),
        registration_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        verification_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        admin_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(60))),
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
            post(ok_handler).delete(ok_handler).patch(ok_handler),
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
    let mut req = builder.body(Body::from("{}")).expect("build request");
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:45678".parse::<SocketAddr>().unwrap(),
    ));
    req
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
    let (state, _) = test_state("disabled", SiteVerificationPolicy::Disabled, None).await;
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
    let (state, _) = test_state("optional", SiteVerificationPolicy::Optional, None).await;
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
            .contains("SITE_ORIGIN_DENIED")
    );

    // required: unknown sites are rejected with verification guidance
    let (state, _) = test_state("required", SiteVerificationPolicy::Required, None).await;
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
            .contains("SITE_VERIFICATION_REQUIRED")
    );
}

#[tokio::test]
async fn disabled_write_errors_include_wildcard_cors() {
    let (state, _) = test_state("disabled-errors", SiteVerificationPolicy::Disabled, None).await;
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

    // Body-limit failures happen before the handler and must also carry CORS.
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
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response_origin(&oversized).as_deref(), Some("*"));
}

#[tokio::test]
async fn verified_site_enforces_exact_origins_and_rejects_null() {
    let (state, store) = test_state("verified", SiteVerificationPolicy::Required, None).await;
    store
        .register_site("a1b2c3d4e5f60718a1b2c3d4e5f60718", &token_hash("claim"))
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
    assert!(body_text(denied).await.contains("SITE_ORIGIN_DENIED"));

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
    assert!(body_text(null_origin).await.contains("SITE_ORIGIN_DENIED"));

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
        .register_site(site_id, &token_hash("claim"))
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
    assert!(body_text(bad).await.contains("SITE_SIGNATURE_INVALID"));
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
async fn admin_lifecycle_and_well_known_verification() {
    let (state, _) = test_state(
        "admin",
        SiteVerificationPolicy::Required,
        Some("test-admin-token"),
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

    // Admin list requires the token.
    let unauthorized = router
        .clone()
        .oneshot(request(Method::GET, "/api/v1/admin/sites", None, &[]))
        .await
        .expect("call router");
    assert_eq!(unauthorized.status(), StatusCode::FORBIDDEN);

    let listed = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/admin/sites",
            None,
            &[("authorization", "Bearer test-admin-token".to_string())],
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

    // Admin: rotate, export a config snippet, revoke secret, revoke origin.
    let rotated = router
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/admin/sites/{site_id}/secret/rotate"),
            None,
            &[("authorization", "Bearer test-admin-token".to_string())],
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
            &format!("/api/v1/admin/sites/{site_id}/config-snippet"),
            None,
            &[("authorization", "Bearer test-admin-token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(snippet.status(), StatusCode::OK);
    assert!(body_text(snippet).await.contains("auth_mode"));

    let revoked_secret = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            &format!("/api/v1/admin/sites/{site_id}/secret"),
            None,
            &[("authorization", "Bearer test-admin-token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(revoked_secret.status(), StatusCode::OK);

    let revoked_origin = router
        .clone()
        .oneshot(
            request(
                Method::POST,
                &format!("/api/v1/admin/sites/{site_id}/origins/revoke"),
                None,
                &[("authorization", "Bearer test-admin-token".to_string())],
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
async fn admin_can_rotate_claim_token() {
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
            &format!("/api/v1/admin/sites/{site_id}/claim-token/rotate"),
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
async fn admin_lists_blocked_rooms() {
    let (state, store) = test_state(
        "blocked-rooms",
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
        .mark_room_blocked("!room:hs", "Refusing to adopt room")
        .await
        .expect("mark blocked");

    let router = cumments_api::build_router(state);
    let resp = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/admin/rooms/blocked",
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
        json["data"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("Refusing to adopt")
    );

    let filtered = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/admin/rooms/blocked?site_id=other",
            None,
            &[("authorization", "Bearer token".to_string())],
        ))
        .await
        .expect("call router");
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered_json: serde_json::Value =
        serde_json::from_str(&body_text(filtered).await).expect("parse response");
    assert_eq!(filtered_json["data"].as_array().map(Vec::len), Some(0));
    assert_eq!(filtered_json["meta"]["total"], 0);
}

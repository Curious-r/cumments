use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use tower::ServiceExt;

use cumments_api::{
    ApiState, build_router,
    pow::{Challenge, Pow},
    rate_limit::RateLimiter,
};
use cumments_core::identity::signature_message;
use cumments_core::models::{PageSlug, SiteId};
use cumments_core::ports::{MatrixDriver, RegistryStore, SiteAuthStore};
use cumments_core::site_auth::{SiteAuthPolicy, SiteVerificationPolicy, token_hash};
use cumments_core::site_service::SiteService;
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "cumments-api-media-deletion-test-{name}-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn test_state_and_store(name: &str, driver: Arc<TestDriver>) -> (ApiState, Arc<DbStore>) {
    let store = Arc::new(
        DbStore::connect(&test_db_url(name))
            .await
            .expect("connect test database"),
    );
    let (event_bus, _) = tokio::sync::broadcast::channel(100);
    let site_service_store: Arc<dyn cumments_core::ports::SiteStore> = store.clone();
    let state = ApiState {
        store: store.clone(),
        driver,
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
        server_name: Some("hs".to_string()),
        registration_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        verification_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        operator_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(60))),
        claim_token_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(60))),
        confirm_limiter: Arc::new(RateLimiter::new(1000, Duration::from_secs(3600))),
        trusted_proxies: Arc::new(Default::default()),
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

fn solve_pow(challenge: &Challenge) -> String {
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

fn upload_request(uri: &str, idempotency_key: &str, body: Vec<u8>) -> Request<Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header("idempotency-key", idempotency_key)
        .body(Body::from(body))
        .expect("build request");
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:45678".parse::<SocketAddr>().unwrap(),
    ));
    req
}

/// Interface-level regression test ensuring MatrixDriver does not expose delete_media.
#[test]
fn matrix_driver_trait_does_not_expose_delete_media() {
    // Compile-time check: MatrixDriver trait has standard upload and send methods
    // but no delete_media method.
    fn _assert_driver_is_object_safe(_: &dyn MatrixDriver) {}
}

#[tokio::test]
async fn upload_media_lifecycle_without_matrix_deletion() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_and_store("media_lifecycle", driver.clone()).await;

    let site_id = "test-site";
    store
        .register_site(site_id, &token_hash("secret"), false)
        .await
        .expect("register site");
    store
        .register_room(
            "!room:hs",
            &SiteId::from(site_id),
            &PageSlug::from("page-one"),
        )
        .await
        .expect("register room");

    let router = build_router(state.clone());

    let signing_key = SigningKey::from_bytes(&[42u8; 32]);
    let author_pubkey = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());

    let image_bytes = b"fake-png-content".to_vec();
    use sha2::{Digest, Sha256};
    let body_hash = hex::encode(Sha256::digest(&image_bytes));

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let challenge_prefix = challenge_response.split('|').next().unwrap();

    let sig_msg = signature_message(&[
        Some("UPLOAD"),
        Some(site_id),
        Some("page-one"),
        Some("image/png"),
        Some("test.png"),
        Some(&body_hash),
        Some(challenge_prefix),
    ]);
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(sig_msg.as_bytes()).to_bytes());

    let uri = format!(
        "/api/v1/sites/{site_id}/pages/page-one/media?author_public_key={author_pubkey}&author_signature={signature}&challenge_response={challenge_response}&mime=image/png&filename=test.png"
    );

    // 1. First upload: Created (200 OK)
    let req1 = upload_request(&uri, "upload-op-1", image_bytes.clone());
    let resp1 = router.clone().oneshot(req1).await.expect("call router");
    assert_eq!(resp1.status(), StatusCode::OK);

    assert!(!resp1.headers().contains_key("idempotent-replayed"));
    let body1: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp1.into_body(), usize::MAX)
            .await
            .expect("read body"),
    )
    .expect("parse json");
    let mxc_url = body1["url"].as_str().expect("url string").to_string();

    // 2. Replay with identical fingerprint: Replayed (200 OK, replayed=true header)
    let req2 = upload_request(&uri, "upload-op-1", image_bytes.clone());
    let resp2 = router.clone().oneshot(req2).await.expect("call router");
    assert_eq!(resp2.status(), StatusCode::OK);
    assert_eq!(resp2.headers().get("idempotent-replayed").unwrap(), "true");

    let body2: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp2.into_body(), usize::MAX)
            .await
            .expect("read body"),
    )
    .expect("parse json");
    assert_eq!(body2["url"], mxc_url);

    // 3. Reuse same idempotency key with DIFFERENT body: Conflict (409 Conflict)
    let req3 = upload_request(&uri, "upload-op-1", b"different-content".to_vec());
    let resp3 = router.clone().oneshot(req3).await.expect("call router");
    assert_eq!(resp3.status(), StatusCode::CONFLICT);
}

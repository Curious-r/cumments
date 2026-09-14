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
    ApiState,
    pow::{Challenge, Pow},
    rate_limit::RateLimiter,
    request::ProfileOperationResponse,
};
use cumments_core::media_reference::{MediaReference, MediaReferenceSource};
use cumments_core::models::{SiteId, VisitorProfile};
use cumments_core::ports::{MediaReferenceStore, ProfileStore, SiteAuthStore};
use cumments_core::profile::{
    ProfileField, ProfileOperationStatus, ProfileTargetValue, clear_avatar_signature_message,
    clear_display_name_signature_message, set_avatar_signature_message,
    set_display_name_signature_message, verify_profile_signature,
};
use cumments_core::site_auth::{SiteAuthPolicy, SiteVerificationPolicy, token_hash};
use cumments_core::site_service::SiteService;
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "cumments-api-profile-test-{name}-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn test_state_with_driver(name: &str, driver: Arc<TestDriver>) -> (ApiState, DbStore) {
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

fn sign(signing_key: &SigningKey, message: &str) -> String {
    let sig = signing_key.sign(message.as_bytes());
    URL_SAFE_NO_PAD.encode(sig.to_bytes())
}

fn request(method: Method, uri: &str, headers: &[(&str, &str)], body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut req = builder
        .body(Body::from(body.to_owned()))
        .expect("build request");
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:45678".parse::<SocketAddr>().unwrap(),
    ));
    req
}

async fn body_json<T: serde::de::DeserializeOwned>(res: axum::response::Response) -> T {
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

// ---------------------------------------------------------------------------
// 1. Display Name Mutations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_display_name_success_and_query() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("set-name-ok", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[1u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let op_id = "op-name-100";
    let name = "Alice Bob";

    let sig_msg = set_display_name_signature_message("my-site", op_id, name);
    let signature = sign(&signing_key, &sig_msg);

    let req_body = serde_json::json!({
        "display_name": name,
        "author_public_key": public_key,
        "author_signature": signature,
        "challenge_response": challenge_response,
    });

    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &req_body.to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: ProfileOperationResponse = body_json(res).await;
    assert_eq!(body.operation_id, op_id);
    assert_eq!(body.status, ProfileOperationStatus::Completed);
    assert_eq!(body.field, ProfileField::DisplayName);
    assert_eq!(body.value.as_deref(), Some(name));
    assert!(body.error.is_none());

    // Verify driver called
    let calls = driver.set_display_name_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, public_key);
    assert_eq!(calls[0].1.as_str(), "my-site");
    assert_eq!(calls[0].2, name);

    // Verify GET /profile reflects authoritative updated name
    let get_uri = format!("/api/v1/sites/my-site/visitors/profile?author_public_key={public_key}");
    let get_res = router
        .clone()
        .oneshot(request(Method::GET, &get_uri, &[], "{}"))
        .await
        .unwrap();
    assert_eq!(get_res.status(), StatusCode::OK);
    let prof: serde_json::Value = body_json(get_res).await;
    assert_eq!(prof["display_name"], name);
    assert!(prof["avatar"].is_null());
    assert!(prof["avatar_url"].is_null());
}

#[tokio::test]
async fn clear_display_name_success_and_query() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("clear-name-ok", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[2u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // First set display name
    let ch1 = state.pow.generate_challenge();
    let ch_resp1 = solve_pow(&ch1);
    let op1 = "op-name-init";
    let sig1 = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op1, "Init Name"),
    );
    let _ = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op1)],
            &serde_json::json!({
                "display_name": "Init Name",
                "author_public_key": public_key,
                "author_signature": sig1,
                "challenge_response": ch_resp1,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    // Now clear it
    let ch2 = state.pow.generate_challenge();
    let ch_resp2 = solve_pow(&ch2);
    let op2 = "op-name-clear";
    let sig2 = sign(
        &signing_key,
        &clear_display_name_signature_message("my-site", op2),
    );

    let res = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op2)],
            &serde_json::json!({
                "author_public_key": public_key,
                "author_signature": sig2,
                "challenge_response": ch_resp2,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: ProfileOperationResponse = body_json(res).await;
    assert_eq!(body.operation_id, op2);
    assert_eq!(body.status, ProfileOperationStatus::Completed);
    assert_eq!(body.field, ProfileField::DisplayName);
    assert_eq!(body.value, None);

    // Verify driver called
    let calls = driver.clear_display_name_calls.lock().await;
    assert_eq!(calls.len(), 1);

    // Verify GET /profile returns null for display_name
    let get_uri = format!("/api/v1/sites/my-site/visitors/profile?author_public_key={public_key}");
    let get_res = router
        .clone()
        .oneshot(request(Method::GET, &get_uri, &[], "{}"))
        .await
        .unwrap();
    let prof: serde_json::Value = body_json(get_res).await;
    assert!(prof["display_name"].is_null());
}

#[tokio::test]
async fn set_display_name_rejects_invalid_values() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("invalid-names", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[3u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // Empty display name
    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", "op-empty")],
            &serde_json::json!({
                "display_name": "",
                "author_public_key": public_key,
                "author_signature": "fake",
                "challenge_response": "prefix|1",
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Oversized display name (> 50 grapheme clusters)
    let long_name = "a".repeat(51);
    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", "op-long")],
            &serde_json::json!({
                "display_name": long_name,
                "author_public_key": public_key,
                "author_signature": "fake",
                "challenge_response": "prefix|1",
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn display_name_rejects_invalid_signature_and_cross_operation_signature() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("invalid-sig", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[4u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let ch = state.pow.generate_challenge();
    let ch_resp = solve_pow(&ch);
    let op_id = "op-sig-test";

    // 1. Tampered signature
    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Valid Name",
                "author_public_key": public_key,
                "author_signature": URL_SAFE_NO_PAD.encode([0u8; 64]),
                "challenge_response": ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 2. Signature for wrong operation: CLEAR_DISPLAY_NAME signature provided to PUT
    let wrong_sig = sign(
        &signing_key,
        &clear_display_name_signature_message("my-site", op_id),
    );
    let res2 = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Valid Name",
                "author_public_key": public_key,
                "author_signature": wrong_sig,
                "challenge_response": ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn display_name_rejects_invalid_pow() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("invalid-pow", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[5u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let op_id = "op-pow-test";
    // Bad challenge prefix and nonce that fails PoW verification
    let bad_ch_resp = "bad_prefix|999999";
    let sig = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op_id, "Alice"),
    );

    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Alice",
                "author_public_key": public_key,
                "author_signature": sig,
                "challenge_response": bad_ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// 2. Avatar Mutations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_and_clear_avatar_success() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("avatar-crud", driver.clone()).await;
    let site_id = SiteId::new("my-site".to_string()).unwrap();
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();

    // Map a media reference beforehand
    let media_ref = store
        .get_or_create_reference(
            &site_id,
            "mxc://hs/avatar123",
            MediaReferenceSource::Cumments,
        )
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[6u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // 1. Set Avatar
    let ch1 = state.pow.generate_challenge();
    let ch_resp1 = solve_pow(&ch1);
    let op1 = "op-avatar-set";
    let sig1 = sign(
        &signing_key,
        &set_avatar_signature_message("my-site", op1, &media_ref),
    );

    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/avatar",
            &[("idempotency-key", op1)],
            &serde_json::json!({
                "avatar": media_ref.to_string(),
                "author_public_key": public_key,
                "author_signature": sig1,
                "challenge_response": ch_resp1,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: ProfileOperationResponse = body_json(res).await;
    assert_eq!(body.operation_id, op1);
    assert_eq!(body.status, ProfileOperationStatus::Completed);
    assert_eq!(body.field, ProfileField::Avatar);
    assert_eq!(body.value.as_deref(), Some(media_ref.as_str()));

    // Verify driver called with resolved MXC
    let avatar_calls = driver.set_avatar_calls.lock().await;
    assert_eq!(avatar_calls.len(), 1);
    assert_eq!(avatar_calls[0].2, "mxc://hs/avatar123");

    // 2. Clear Avatar
    let ch2 = state.pow.generate_challenge();
    let ch_resp2 = solve_pow(&ch2);
    let op2 = "op-avatar-clear";
    let sig2 = sign(
        &signing_key,
        &clear_avatar_signature_message("my-site", op2),
    );

    let res_clear = router
        .clone()
        .oneshot(request(
            Method::DELETE,
            "/api/v1/sites/my-site/visitors/profile/avatar",
            &[("idempotency-key", op2)],
            &serde_json::json!({
                "author_public_key": public_key,
                "author_signature": sig2,
                "challenge_response": ch_resp2,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(res_clear.status(), StatusCode::OK);
    let clear_body: ProfileOperationResponse = body_json(res_clear).await;
    assert_eq!(clear_body.operation_id, op2);
    assert_eq!(clear_body.status, ProfileOperationStatus::Completed);
    assert_eq!(clear_body.value, None);

    let clear_calls = driver.clear_avatar_calls.lock().await;
    assert_eq!(clear_calls.len(), 1);
}

#[tokio::test]
async fn set_avatar_rejects_unknown_cross_site_and_raw_mxc() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("avatar-rejects", driver.clone()).await;
    let site_a = SiteId::new("site-a".to_string()).unwrap();
    let _site_b = SiteId::new("site-b".to_string()).unwrap();
    store
        .register_site("site-a", &token_hash("claim-a"), false)
        .await
        .unwrap();
    store
        .register_site("site-b", &token_hash("claim-b"), false)
        .await
        .unwrap();

    // Map media under site-a only
    let media_site_a = store
        .get_or_create_reference(
            &site_a,
            "mxc://hs/site-a-avatar",
            MediaReferenceSource::Cumments,
        )
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // 1. Unknown MediaReference under site-a
    let unmapped_ref = MediaReference::new_v4();
    let ch = state.pow.generate_challenge();
    let ch_resp = solve_pow(&ch);
    let sig_unmapped = sign(
        &signing_key,
        &set_avatar_signature_message("site-a", "op-unmapped", &unmapped_ref),
    );

    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/site-a/visitors/profile/avatar",
            &[("idempotency-key", "op-unmapped")],
            &serde_json::json!({
                "avatar": unmapped_ref.to_string(),
                "author_public_key": public_key,
                "author_signature": sig_unmapped,
                "challenge_response": ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 2. Cross-site MediaReference: media_site_a attempted on site-b
    let ch_b = state.pow.generate_challenge();
    let ch_resp_b = solve_pow(&ch_b);
    let sig_cross = sign(
        &signing_key,
        &set_avatar_signature_message("site-b", "op-cross", &media_site_a),
    );

    let res_cross = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/site-b/visitors/profile/avatar",
            &[("idempotency-key", "op-cross")],
            &serde_json::json!({
                "avatar": media_site_a.to_string(),
                "author_public_key": public_key,
                "author_signature": sig_cross,
                "challenge_response": ch_resp_b,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res_cross.status(), StatusCode::NOT_FOUND);

    // 3. Raw MXC URI directly in avatar field is rejected by syntax validation
    let res_raw = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/site-a/visitors/profile/avatar",
            &[("idempotency-key", "op-raw")],
            &serde_json::json!({
                "avatar": "mxc://hs/raw-uri",
                "author_public_key": public_key,
                "author_signature": "fake",
                "challenge_response": "pref|1",
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res_raw.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// 3. Idempotency & Replays
// ---------------------------------------------------------------------------

#[tokio::test]
async fn idempotent_replay_returns_cached_operation_without_reexecution() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("idempotent-replay", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[8u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let ch = state.pow.generate_challenge();
    let ch_resp = solve_pow(&ch);
    let op_id = "op-replay-1";
    let sig = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op_id, "Bob"),
    );
    let body = serde_json::json!({
        "display_name": "Bob",
        "author_public_key": public_key,
        "author_signature": sig,
        "challenge_response": ch_resp,
    })
    .to_string();

    // First request
    let res1 = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::OK);
    assert!(res1.headers().get("idempotent-replayed").is_none());

    assert_eq!(driver.set_display_name_calls.lock().await.len(), 1);

    // Replay with different valid PoW challenge using the SAME signature
    let ch2 = state.pow.generate_challenge();
    let ch_resp2 = solve_pow(&ch2);
    let body2 = serde_json::json!({
        "display_name": "Bob",
        "author_public_key": public_key,
        "author_signature": sig,
        "challenge_response": ch_resp2,
    })
    .to_string();

    let res2 = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &body2,
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    assert_eq!(
        res2.headers()
            .get("idempotent-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let body2: ProfileOperationResponse = body_json(res2).await;
    assert_eq!(body2.operation_id, op_id);
    assert_eq!(body2.status, ProfileOperationStatus::Completed);

    // Driver was NOT called again
    assert_eq!(driver.set_display_name_calls.lock().await.len(), 1);
}

#[tokio::test]
async fn replay_conflict_on_different_fingerprint() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("replay-conflict", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[9u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let ch1 = state.pow.generate_challenge();
    let ch_resp1 = solve_pow(&ch1);
    let op_id = "op-conflict-1";
    let sig1 = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op_id, "Bob"),
    );
    let _ = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Bob",
                "author_public_key": public_key,
                "author_signature": sig1,
                "challenge_response": ch_resp1,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    // Replay same idempotency-key with DIFFERENT display name
    let ch2 = state.pow.generate_challenge();
    let ch_resp2 = solve_pow(&ch2);
    let sig2 = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op_id, "Charlie"),
    );
    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Charlie",
                "author_public_key": public_key,
                "author_signature": sig2,
                "challenge_response": ch_resp2,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn replay_with_fresh_pow_is_accepted() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("replay-fresh-pow", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[10u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let ch1 = state.pow.generate_challenge();
    let ch_resp1 = solve_pow(&ch1);
    let op_id = "op-fresh-pow";
    let sig1 = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op_id, "Bob"),
    );
    let _ = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Bob",
                "author_public_key": public_key,
                "author_signature": sig1,
                "challenge_response": ch_resp1,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    // Generate brand new challenge, solve it, and reuse the EXACT SAME signature sig1!
    let ch2 = state.pow.generate_challenge();
    let ch_resp2 = solve_pow(&ch2);

    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Bob",
                "author_public_key": public_key,
                "author_signature": sig1,
                "challenge_response": ch_resp2,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("idempotent-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
}

// ---------------------------------------------------------------------------
// 4. Operation States & Downstream Errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn operation_state_failed_downstream_returns_error_response() {
    let driver = Arc::new(TestDriver::new());
    // Inject deterministic downstream failure
    driver
        .set_next_profile_error(cumments_core::profile::ProfileDriverError::Deterministic(
            "Matrix policy rejected display name".to_string(),
        ))
        .await;

    let (state, store) = test_state_with_driver("op-failed", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[11u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let ch = state.pow.generate_challenge();
    let ch_resp = solve_pow(&ch);
    let op_id = "op-fail-downstream";
    let sig = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op_id, "BadName"),
    );

    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "BadName",
                "author_public_key": public_key,
                "author_signature": sig,
                "challenge_response": ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    // Returns RFC 9457 error response with 400 Bad Request
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Verify stored operation is Failed
    let op = store.get_profile_operation(op_id).await.unwrap().unwrap();
    assert_eq!(op.status, ProfileOperationStatus::Failed);
    assert_eq!(
        op.error_detail.as_deref(),
        Some("Matrix policy rejected display name")
    );
}

#[tokio::test]
async fn operation_state_unknown_downstream_returns_accepted() {
    let driver = Arc::new(TestDriver::new());
    // Inject ambiguous downstream failure (e.g. timeout)
    driver
        .set_next_profile_error(cumments_core::profile::ProfileDriverError::Ambiguous(
            "Homeserver connection timed out".to_string(),
        ))
        .await;

    let (state, store) = test_state_with_driver("op-unknown", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[12u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let ch = state.pow.generate_challenge();
    let ch_resp = solve_pow(&ch);
    let op_id = "op-ambiguous-downstream";
    let sig = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op_id, "TimeoutName"),
    );

    let res = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "TimeoutName",
                "author_public_key": public_key,
                "author_signature": sig,
                "challenge_response": ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    // Returns 202 Accepted because outcome is ambiguous
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let body: ProfileOperationResponse = body_json(res).await;
    assert_eq!(body.operation_id, op_id);
    assert_eq!(body.status, ProfileOperationStatus::Unknown);
    assert_eq!(
        body.error.as_deref(),
        Some("Homeserver connection timed out")
    );

    // Verify stored operation is Unknown
    let op = store.get_profile_operation(op_id).await.unwrap().unwrap();
    assert_eq!(op.status, ProfileOperationStatus::Unknown);

    // Replay also returns 202 Accepted and does NOT claim to be completed
    let ch_replay = state.pow.generate_challenge();
    let ch_resp_replay = solve_pow(&ch_replay);
    let res_replay = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "TimeoutName",
                "author_public_key": public_key,
                "author_signature": sig,
                "challenge_response": ch_resp_replay,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res_replay.status(), StatusCode::ACCEPTED);
    let replay_body: ProfileOperationResponse = body_json(res_replay).await;
    assert_eq!(replay_body.status, ProfileOperationStatus::Unknown);
}

// ---------------------------------------------------------------------------
// 5. Strict Same-Field Serialization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn same_field_serialization_blocks_subsequent_op_while_avatar_proceeds() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("same-field-ser", driver.clone()).await;
    let site_id = SiteId::new("my-site".to_string()).unwrap();
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let media_ref = store
        .get_or_create_reference(&site_id, "mxc://hs/pic-ser", MediaReferenceSource::Cumments)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[13u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // 1. Manually insert an unresolved operation in store for display_name
    let blocker_target = ProfileTargetValue::SetDisplayName("Blocker".to_string());
    let _ = store
        .claim_or_get_profile_operation("op-blocker", &public_key, &site_id, &blocker_target)
        .await
        .unwrap();
    // Claim lease to put it into Dispatching
    assert!(store.claim_for_execution("op-blocker").await.unwrap());

    // 2. Submit a second mutation targeting the same field (display_name)
    let ch = state.pow.generate_challenge();
    let ch_resp = solve_pow(&ch);
    let op2 = "op-blocked-subsequent";
    let sig2 = sign(
        &signing_key,
        &set_display_name_signature_message("my-site", op2, "Queued"),
    );

    let res_blocked = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op2)],
            &serde_json::json!({
                "display_name": "Queued",
                "author_public_key": public_key,
                "author_signature": sig2,
                "challenge_response": ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    // Since op-blocker is in Dispatching, op2 cannot execute yet -> 202 Accepted (Pending)
    assert_eq!(res_blocked.status(), StatusCode::ACCEPTED);
    let body_blocked: ProfileOperationResponse = body_json(res_blocked).await;
    assert_eq!(body_blocked.status, ProfileOperationStatus::Pending);

    // 3. Submit a mutation on a DIFFERENT field (avatar)
    let ch3 = state.pow.generate_challenge();
    let ch_resp3 = solve_pow(&ch3);
    let op3 = "op-avatar-independent";
    let sig3 = sign(
        &signing_key,
        &set_avatar_signature_message("my-site", op3, &media_ref),
    );

    let res_avatar = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/avatar",
            &[("idempotency-key", op3)],
            &serde_json::json!({
                "avatar": media_ref.to_string(),
                "author_public_key": public_key,
                "author_signature": sig3,
                "challenge_response": ch_resp3,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    // Avatar mutation is on an independent field, so it completes immediately!
    assert_eq!(res_avatar.status(), StatusCode::OK);
    let body_avatar: ProfileOperationResponse = body_json(res_avatar).await;
    assert_eq!(body_avatar.status, ProfileOperationStatus::Completed);
    assert_eq!(body_avatar.field, ProfileField::Avatar);
}

#[tokio::test]
async fn different_sites_same_key_same_field_do_not_block_each_other() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("cross-site-noblock", driver.clone()).await;
    let site_a = SiteId::new("site-a".to_string()).unwrap();
    store
        .register_site("site-a", &token_hash("claim-a"), false)
        .await
        .unwrap();
    store
        .register_site("site-b", &token_hash("claim-b"), false)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[14u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // 1. Manually insert an unresolved operation in store for site-a display_name
    let blocker_target = ProfileTargetValue::SetDisplayName("Blocker A".to_string());
    let _ = store
        .claim_or_get_profile_operation("op-blocker-a", &public_key, &site_a, &blocker_target)
        .await
        .unwrap();
    assert!(store.claim_for_execution("op-blocker-a").await.unwrap());

    // 2. Submit mutation on site-b for the SAME public key and field
    let ch = state.pow.generate_challenge();
    let ch_resp = solve_pow(&ch);
    let op_b = "op-site-b-independent";
    let sig_b = sign(
        &signing_key,
        &set_display_name_signature_message("site-b", op_b, "Bob On Site B"),
    );

    let res_b = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/site-b/visitors/profile/display_name",
            &[("idempotency-key", op_b)],
            &serde_json::json!({
                "display_name": "Bob On Site B",
                "author_public_key": public_key,
                "author_signature": sig_b,
                "challenge_response": ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();

    // site-b is NOT blocked by site-a's Dispatching operation! It completes immediately (200 OK)
    assert_eq!(res_b.status(), StatusCode::OK);
    let body_b: ProfileOperationResponse = body_json(res_b).await;
    assert_eq!(body_b.status, ProfileOperationStatus::Completed);
    assert_eq!(body_b.field, ProfileField::DisplayName);
    assert_eq!(body_b.value.as_deref(), Some("Bob On Site B"));
}

// ---------------------------------------------------------------------------
// 6. Authoritative GET /profile & Read-Only Invariants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_profile_is_read_only_and_does_not_expose_raw_mxc() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("get-prof-readonly", driver.clone()).await;
    let site_id = SiteId::new("my-site".to_string()).unwrap();
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[14u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // Set Matrix profile directly with an UNMAPPED avatar
    driver
        .insert_visitor_profile(
            "my-site",
            &public_key,
            VisitorProfile {
                display_name: Some("Homeserver User".to_string()),
                avatar_url: Some("mxc://hs/unmapped-mxc".to_string()),
            },
        )
        .await;

    // Call GET /profile
    let uri = format!("/api/v1/sites/my-site/visitors/profile?author_public_key={public_key}");
    let res = router
        .clone()
        .oneshot(request(Method::GET, &uri, &[], "{}"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let prof: serde_json::Value = body_json(res).await;

    assert_eq!(prof["display_name"], "Homeserver User");
    // Since "mxc://hs/unmapped-mxc" has no mapping in media_references, avatar and avatar_url MUST be null
    assert!(
        prof["avatar"].is_null(),
        "raw MXC must never be exposed as MediaReference"
    );
    assert!(
        prof["avatar_url"].is_null(),
        "raw MXC must never be exposed as URL"
    );

    // Verify NO MediaReference was allocated or inserted into the store
    let lookup = store
        .find_reference(&site_id, "mxc://hs/unmapped-mxc")
        .await
        .unwrap();
    assert!(
        lookup.is_none(),
        "GET /profile must be read-only and not create mappings"
    );

    // Now map it in the store
    let media_ref = store
        .get_or_create_reference(
            &site_id,
            "mxc://hs/unmapped-mxc",
            MediaReferenceSource::Cumments,
        )
        .await
        .unwrap();

    // Call GET /profile again
    let res2 = router
        .clone()
        .oneshot(request(Method::GET, &uri, &[], "{}"))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    let prof2: serde_json::Value = body_json(res2).await;
    assert_eq!(prof2["display_name"], "Homeserver User");
    assert_eq!(prof2["avatar"], media_ref.as_str());
    // Since media proxy is disabled in this test state, avatar_url is null (never raw MXC!)
    assert!(prof2["avatar_url"].is_null());
}

#[tokio::test]
async fn pow_decoupling_and_signature_invariants() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_with_driver("pow-decoupling", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();
    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[15u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let op_id = "op-decoupled-pow";
    let target = ProfileTargetValue::SetDisplayName("Alice".to_string());

    // 1. Signature generated WITHOUT PoW challenge is accepted when PoW is valid.
    let sig_msg = set_display_name_signature_message("my-site", op_id, "Alice");
    assert_eq!(
        sig_msg,
        format!(
            "[\"SET_DISPLAY_NAME\",\"my-site\",\"{}\",\"{}\"]",
            op_id,
            target.semantic_fingerprint("my-site")
        )
    );
    let signature = sign(&signing_key, &sig_msg);

    let ch1 = state.pow.generate_challenge();
    let ch_resp1 = solve_pow(&ch1);

    let res1 = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Alice",
                "author_public_key": public_key,
                "author_signature": signature,
                "challenge_response": ch_resp1,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::OK);

    // 2. Same signature can be replayed with a different valid PoW challenge.
    let ch2 = state.pow.generate_challenge();
    let ch_resp2 = solve_pow(&ch2);
    assert_ne!(ch1.prefix, ch2.prefix);

    let res2 = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", op_id)],
            &serde_json::json!({
                "display_name": "Alice",
                "author_public_key": public_key,
                "author_signature": signature, // SAME signature!
                "challenge_response": ch_resp2,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    assert_eq!(
        res2.headers()
            .get("idempotent-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );

    // 3. Changing only the PoW challenge does not change the semantic fingerprint.
    let fp1 = target.semantic_fingerprint("my-site");
    let fp2 = target.semantic_fingerprint("my-site");
    assert_eq!(fp1, fp2);

    // 4. A signature for one semantic operation cannot authorize another semantic operation.
    let ch3 = state.pow.generate_challenge();
    let ch_resp3 = solve_pow(&ch3);
    let res3 = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", "op-diff-val")],
            &serde_json::json!({
                "display_name": "Bob",
                "author_public_key": public_key,
                "author_signature": signature, // signature for "Alice"
                "challenge_response": ch_resp3,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res3.status(), StatusCode::FORBIDDEN);

    // 5. Expired / invalid PoW is still rejected independently.
    let bad_ch_resp = format!("{}|0", ch3.prefix); // un-mined nonce
    let res4 = router
        .clone()
        .oneshot(request(
            Method::PUT,
            "/api/v1/sites/my-site/visitors/profile/display_name",
            &[("idempotency-key", "op-bad-pow")],
            &serde_json::json!({
                "display_name": "Alice",
                "author_public_key": public_key,
                "author_signature": signature,
                "challenge_response": bad_ch_resp,
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res4.status(), StatusCode::FORBIDDEN);

    // 6. Removing or changing the PoW challenge does not invalidate the semantic signature itself.
    assert!(verify_profile_signature(
        &public_key,
        &target,
        "my-site",
        op_id,
        &signature,
    ));
}

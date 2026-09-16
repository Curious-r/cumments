//! Site-scoped avatar upload and profile-avatar provenance.
//!
//! Covers the restored avatar upload producer (`POST
//! .../visitors/profile/avatar/media`) and the write-admission check that
//! `PUT .../visitors/profile/avatar` now requires: only a site-scoped,
//! same-visitor, same-site avatar upload may authorize an avatar mutation.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use cumments_api::{
    ApiState, build_router,
    pow::{Challenge, Pow},
    rate_limit::RateLimiter,
};
use cumments_core::identity::signature_message;
use cumments_core::media_upload::avatar_upload_signature_message;
use cumments_core::ports::{MessageStore, ProfileStore, SiteAuthStore};
use cumments_core::profile::{
    ProfileOperationStatus, ProfileTargetValue, clear_avatar_signature_message,
    set_avatar_signature_message,
};
use cumments_core::site_auth::{SiteAuthPolicy, SiteVerificationPolicy, token_hash};
use cumments_core::site_service::SiteService;
use cumments_store::DbStore;
use cumments_store::sea_orm::{ConnectionTrait, Statement};
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "cumments-api-avatar-upload-test-{name}-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn test_state_and_store(
    name: &str,
    driver: Arc<TestDriver>,
) -> (ApiState, Arc<DbStore>, String) {
    let db_url = test_db_url(name);
    let store = Arc::new(
        DbStore::connect(&db_url)
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
    (state, store, db_url)
}

fn solve_pow(challenge: &Challenge) -> String {
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
    URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

fn public_key(signing_key: &SigningKey) -> String {
    URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes())
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

fn json_request(method: Method, uri: &str, op_id: &str, body: &str) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", op_id)
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

const UPLOAD_MIME: &str = "image/png";
const UPLOAD_FILENAME: &str = "avatar.png";

async fn do_avatar_upload(
    state: &ApiState,
    router: &axum::Router,
    site_id: &str,
    signing_key: &SigningKey,
    bytes: &[u8],
    idempotency_key: &str,
) -> axum::response::Response {
    let author = public_key(signing_key);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let challenge_prefix = challenge_response.split('|').next().unwrap();
    let body_hash = hex::encode(Sha256::digest(bytes));
    let message = avatar_upload_signature_message(
        site_id,
        UPLOAD_MIME,
        UPLOAD_FILENAME,
        &body_hash,
        challenge_prefix,
    );
    let signature = sign(signing_key, &message);
    let uri = format!(
        "/api/v1/sites/{site_id}/visitors/profile/avatar/media?author_public_key={author}\
         &author_signature={signature}&challenge_response={challenge_response}\
         &mime={UPLOAD_MIME}&filename={UPLOAD_FILENAME}"
    );
    router
        .clone()
        .oneshot(upload_request(&uri, idempotency_key, bytes.to_vec()))
        .await
        .expect("call router")
}

async fn do_page_media_upload(
    state: &ApiState,
    router: &axum::Router,
    site_id: &str,
    page_slug: &str,
    signing_key: &SigningKey,
    bytes: &[u8],
    idempotency_key: &str,
) -> axum::response::Response {
    let author = public_key(signing_key);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let challenge_prefix = challenge_response.split('|').next().unwrap();
    let body_hash = hex::encode(Sha256::digest(bytes));
    let message = signature_message(&[
        Some("UPLOAD"),
        Some(site_id),
        Some(page_slug),
        Some(UPLOAD_MIME),
        Some(UPLOAD_FILENAME),
        Some(&body_hash),
        Some(challenge_prefix),
    ]);
    let signature = sign(signing_key, &message);
    let uri = format!(
        "/api/v1/sites/{site_id}/pages/{page_slug}/media?author_public_key={author}\
         &author_signature={signature}&challenge_response={challenge_response}\
         &mime={UPLOAD_MIME}&filename={UPLOAD_FILENAME}"
    );
    router
        .clone()
        .oneshot(upload_request(&uri, idempotency_key, bytes.to_vec()))
        .await
        .expect("call router")
}

async fn do_set_avatar(
    state: &ApiState,
    router: &axum::Router,
    site_id: &str,
    signing_key: &SigningKey,
    mxc: &str,
    op_id: &str,
) -> axum::response::Response {
    let author = public_key(signing_key);
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let signature = sign(
        signing_key,
        &set_avatar_signature_message(site_id, op_id, mxc),
    );
    let uri = format!("/api/v1/sites/{site_id}/visitors/profile/avatar");
    router
        .clone()
        .oneshot(json_request(
            Method::PUT,
            &uri,
            op_id,
            &serde_json::json!({
                "avatar": mxc,
                "author_public_key": author,
                "author_signature": signature,
                "challenge_response": challenge_response,
            })
            .to_string(),
        ))
        .await
        .expect("call router")
}

async fn upload_mxc(response: axum::response::Response) -> String {
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = body_json(response).await;
    body["url"].as_str().expect("url string").to_string()
}

// ---------------------------------------------------------------------------
// 1-2. Recording and response shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn avatar_upload_records_site_scoped_provenance() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, db_url) = test_state_and_store("avatar-provenance", driver).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[42u8; 32]);
    let author = public_key(&signing_key);

    let mxc = upload_mxc(
        do_avatar_upload(
            &state,
            &router,
            "test-site",
            &signing_key,
            b"fake-png-content",
            "avatar-upload-op-1",
        )
        .await,
    )
    .await;

    assert!(
        store
            .avatar_upload_owned_by(&mxc, &author, "test-site")
            .await
            .unwrap(),
        "the avatar upload must be recorded as site-scoped provenance"
    );
    assert!(
        !store
            .media_upload_owned_by(&mxc, &author, "test-site", "page-one")
            .await
            .unwrap(),
        "the avatar upload must not be page-scoped"
    );

    // Directly assert the stored row carries no page slug.
    let db = cumments_store::sea_orm::Database::connect(&db_url)
        .await
        .expect("connect raw db");
    let row = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            format!("SELECT page_slug FROM media_uploads WHERE mxc_url = '{mxc}'"),
        ))
        .await
        .expect("query upload")
        .expect("avatar upload row");
    let page_slug: Option<String> = row.try_get("", "page_slug").expect("page_slug");
    assert!(
        page_slug.is_none(),
        "a site-scoped avatar upload must record page_slug = NULL"
    );
}

#[tokio::test]
async fn avatar_upload_returns_matrix_mxc_as_intermediate_value() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) = test_state_and_store("avatar-response", driver).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let author = public_key(&signing_key);
    let bytes = b"fake-png-content";

    let response = do_avatar_upload(
        &state,
        &router,
        "test-site",
        &signing_key,
        bytes,
        "avatar-upload-op-resp",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = body_json(response).await;
    let url = body["url"].as_str().expect("url string");

    // The driver's upload result is returned verbatim: the raw MXC.
    assert_eq!(
        url,
        format!(
            "mxc://hs/test-site/{author}-{UPLOAD_FILENAME}-{}",
            bytes.len()
        )
    );
    assert!(
        url.starts_with("mxc://"),
        "the upload returns the write-side MXC, not a browser media URL"
    );
    assert!(
        !url.contains("/api/v1/media/"),
        "the upload result must not be a signed media-proxy URL"
    );
}

// ---------------------------------------------------------------------------
// 3-7. Avatar mutation provenance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn valid_avatar_upload_can_set_profile_avatar() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) = test_state_and_store("avatar-set-ok", driver.clone()).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[9u8; 32]);
    let author = public_key(&signing_key);

    let mxc = upload_mxc(
        do_avatar_upload(
            &state,
            &router,
            "test-site",
            &signing_key,
            b"valid-avatar",
            "avatar-upload-ok",
        )
        .await,
    )
    .await;

    let op_id = "op-avatar-valid";
    let response = do_set_avatar(&state, &router, "test-site", &signing_key, &mxc, op_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: cumments_api::request::ProfileOperationResponse = body_json(response).await;
    assert_eq!(body.status, ProfileOperationStatus::Completed);

    let avatar_calls = driver.set_avatar_calls.lock().await;
    assert_eq!(avatar_calls.len(), 1);
    assert_eq!(avatar_calls[0].0, author);
    assert_eq!(avatar_calls[0].2, mxc);
}

#[tokio::test]
async fn page_scoped_comment_media_cannot_set_avatar() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) = test_state_and_store("avatar-page-scoped", driver.clone()).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[11u8; 32]);

    let mxc = upload_mxc(
        do_page_media_upload(
            &state,
            &router,
            "test-site",
            "page-one",
            &signing_key,
            b"comment-media",
            "page-media-op-1",
        )
        .await,
    )
    .await;

    let response = do_set_avatar(
        &state,
        &router,
        "test-site",
        &signing_key,
        &mxc,
        "op-avatar-page",
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a page-scoped comment-media upload must not authorize an avatar"
    );
    assert!(driver.set_avatar_calls.lock().await.is_empty());
}

#[tokio::test]
async fn avatar_upload_from_another_visitor_cannot_set_avatar() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) =
        test_state_and_store("avatar-other-visitor", driver.clone()).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let owner = SigningKey::from_bytes(&[13u8; 32]);
    let other = SigningKey::from_bytes(&[14u8; 32]);

    let mxc = upload_mxc(
        do_avatar_upload(
            &state,
            &router,
            "test-site",
            &owner,
            b"owner-avatar",
            "avatar-upload-owner",
        )
        .await,
    )
    .await;

    let response = do_set_avatar(
        &state,
        &router,
        "test-site",
        &other,
        &mxc,
        "op-avatar-other",
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "another visitor's avatar upload must not authorize an avatar"
    );
    assert!(driver.set_avatar_calls.lock().await.is_empty());
}

#[tokio::test]
async fn avatar_upload_from_another_site_cannot_set_avatar() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) = test_state_and_store("avatar-other-site", driver.clone()).await;
    store
        .register_site("site-a", &token_hash("secret-a"), false)
        .await
        .unwrap();
    store
        .register_site("site-b", &token_hash("secret-b"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[15u8; 32]);

    let mxc = upload_mxc(
        do_avatar_upload(
            &state,
            &router,
            "site-a",
            &signing_key,
            b"site-a-avatar",
            "avatar-upload-site-a",
        )
        .await,
    )
    .await;

    let response = do_set_avatar(
        &state,
        &router,
        "site-b",
        &signing_key,
        &mxc,
        "op-avatar-site-b",
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "an avatar upload recorded for another site must not authorize an avatar"
    );
    assert!(driver.set_avatar_calls.lock().await.is_empty());
}

#[tokio::test]
async fn syntactically_valid_but_unrecorded_mxc_is_rejected() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) = test_state_and_store("avatar-unrecorded", driver.clone()).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[17u8; 32]);

    let response = do_set_avatar(
        &state,
        &router,
        "test-site",
        &signing_key,
        "mxc://hs/never-uploaded",
        "op-avatar-unrecorded",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(driver.set_avatar_calls.lock().await.is_empty());
}

// ---------------------------------------------------------------------------
// 8. Upload idempotency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn avatar_upload_idempotency_matches_media_upload_semantics() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) = test_state_and_store("avatar-idempotency", driver).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[19u8; 32]);
    let bytes = b"idempotent-avatar";

    let key = "avatar-idem-key";
    let first = do_avatar_upload(&state, &router, "test-site", &signing_key, bytes, key).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert!(!first.headers().contains_key("idempotent-replayed"));
    let first_url = {
        let body: serde_json::Value = body_json(first).await;
        body["url"].as_str().expect("url").to_string()
    };

    // Identical request: replay returns the same MXC and is marked replayed.
    let replay = do_avatar_upload(&state, &router, "test-site", &signing_key, bytes, key).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers().get("idempotent-replayed").unwrap(), "true");
    let replay_url = {
        let body: serde_json::Value = body_json(replay).await;
        body["url"].as_str().expect("url").to_string()
    };
    assert_eq!(replay_url, first_url);

    // Same key with a different body: conflict.
    let conflict = do_avatar_upload(
        &state,
        &router,
        "test-site",
        &signing_key,
        b"different",
        key,
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// 9. Profile operations carry the MXC directly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn avatar_profile_operation_persists_and_signs_mxc_directly() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) = test_state_and_store("avatar-op-mxc", driver.clone()).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[21u8; 32]);

    let mxc = upload_mxc(
        do_avatar_upload(
            &state,
            &router,
            "test-site",
            &signing_key,
            b"direct-mxc",
            "avatar-upload-direct",
        )
        .await,
    )
    .await;

    let op_id = "op-avatar-direct";
    let response = do_set_avatar(&state, &router, "test-site", &signing_key, &mxc, op_id).await;
    assert_eq!(response.status(), StatusCode::OK);

    let operation = store
        .get_profile_operation(op_id)
        .await
        .expect("read operation")
        .expect("operation persisted");
    assert_eq!(
        operation.target_value,
        ProfileTargetValue::SetAvatar(mxc.clone()),
        "the durable operation must carry the MXC directly, with no reverse lookup"
    );
    assert_eq!(operation.status, ProfileOperationStatus::Completed);

    let avatar_calls = driver.set_avatar_calls.lock().await;
    assert_eq!(avatar_calls.len(), 1);
    assert_eq!(avatar_calls[0].2, mxc);
}

// ---------------------------------------------------------------------------
// 10. Clear avatar is independent of upload provenance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn clear_avatar_is_independent_of_upload_provenance() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) = test_state_and_store("avatar-clear", driver.clone()).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[23u8; 32]);
    let author = public_key(&signing_key);

    let op_id = "op-avatar-clear";
    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);
    let signature = sign(
        &signing_key,
        &clear_avatar_signature_message("test-site", op_id),
    );
    let uri = "/api/v1/sites/test-site/visitors/profile/avatar";
    let response = router
        .clone()
        .oneshot(json_request(
            Method::DELETE,
            uri,
            op_id,
            &serde_json::json!({
                "author_public_key": author,
                "author_signature": signature,
                "challenge_response": challenge_response,
            })
            .to_string(),
        ))
        .await
        .expect("call router");

    assert_eq!(response.status(), StatusCode::OK);
    let body: cumments_api::request::ProfileOperationResponse = body_json(response).await;
    assert_eq!(body.status, ProfileOperationStatus::Completed);
    assert_eq!(body.value, None);

    let clear_calls = driver.clear_avatar_calls.lock().await;
    assert_eq!(clear_calls.len(), 1);
}

// ---------------------------------------------------------------------------
// Failed provenance check leaves no operation and no Matrix write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failed_provenance_creates_no_operation_and_no_matrix_write() {
    let driver = Arc::new(TestDriver::new());
    let (state, store, _db_url) =
        test_state_and_store("avatar-no-side-effects", driver.clone()).await;
    store
        .register_site("test-site", &token_hash("secret"), false)
        .await
        .unwrap();
    let router = build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[25u8; 32]);

    let op_id = "op-avatar-rejected";
    let response = do_set_avatar(
        &state,
        &router,
        "test-site",
        &signing_key,
        "mxc://hs/no-provenance",
        op_id,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    assert!(
        store
            .get_profile_operation(op_id)
            .await
            .expect("read operation")
            .is_none(),
        "a rejected avatar must not leave a durable profile operation"
    );
    assert!(
        driver.set_avatar_calls.lock().await.is_empty(),
        "a rejected avatar must not write to Matrix"
    );
}

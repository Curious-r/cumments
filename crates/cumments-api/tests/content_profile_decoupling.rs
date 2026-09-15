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
use cumments_core::identity::{
    locate_signature_message, post_signature_message, signature_message,
};
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, Message, MessageStatus, PageSlug, SiteId, TextContent,
    TextStyle,
};
use cumments_core::poll::{
    PollSemanticAnswer, PollSemanticKind, poll_semantic_operation, poll_signature_envelope,
};
use cumments_core::ports::{MessageStore, RegistryStore, SiteAuthStore};
use cumments_core::profile::{ProfileOperationStatus, set_display_name_signature_message};
use cumments_core::site_auth::{SiteAuthPolicy, SiteVerificationPolicy, token_hash};
use cumments_core::site_service::SiteService;
use cumments_reconciler::{PassConfig, PostsPass, ReconcilePass, ReconcilerDeps, UpdatesPass};
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "cumments-api-decoupling-test-{name}-{}.db",
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

fn reconciler_deps(store: Arc<DbStore>, driver: Arc<TestDriver>) -> Arc<ReconcilerDeps> {
    Arc::new(ReconcilerDeps {
        submission_store: store.clone(),
        registry_store: store.clone(),
        site_store: store.clone(),
        role_claim_store: store.clone(),
        governance_store: store.clone(),
        projection_repair_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        virtual_user_store: store.clone(),
        site_auth_store: store.clone(),
        site_transfer_store: store.clone(),
        state_redaction_repairer: driver.clone(),
        driver: driver.clone(),
        site_service: Arc::new(SiteService::new(store.clone())),
        profile_store: Some(store.clone()),
    })
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

async fn body_text(res: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

#[allow(clippy::too_many_arguments)]
fn signed_poll_body(
    signing_key: &SigningKey,
    site: &str,
    page: &str,
    operation_id: &str,
    question: &str,
    answers: &[(&str, &str)],
    kind: PollSemanticKind,
    max_selections: u64,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
    challenge_prefix: &str,
    challenge_response: &str,
) -> String {
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

// ---------------------------------------------------------------------------
// 1. Rejection of legacy requests with `display_name`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_content_requests_with_display_name_are_rejected_with_400() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_and_store("legacy-400", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[1u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let challenge = state.pow.generate_challenge();
    let challenge_response = solve_pow(&challenge);

    // 1. POST /comments with legacy `display_name`
    {
        let message = post_signature_message(
            "my-site",
            "page-1",
            "hello world",
            None,
            None,
            &challenge.prefix,
        );
        let signature = sign(&signing_key, &message);
        let body = serde_json::json!({
            "content": "hello world",
            "display_name": "Legacy Eve",
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/comments",
                &[("idempotency-key", "idemp-legacy-post")],
                &body.to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let text = body_text(res).await;
        assert!(
            text.contains("unknown field `display_name`") || text.contains("unknown field"),
            "expected unknown field rejection, got: {text}"
        );
    }

    // 2. POST /location with legacy `display_name`
    {
        let message = locate_signature_message(
            "my-site",
            "page-1",
            "geo:37.7749,-122.4194",
            None,
            None,
            &challenge.prefix,
        );
        let signature = sign(&signing_key, &message);
        let body = serde_json::json!({
            "geo_uri": "geo:37.7749,-122.4194",
            "display_name": "Legacy Eve",
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/location",
                &[("idempotency-key", "idemp-legacy-loc")],
                &body.to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let text = body_text(res).await;
        assert!(
            text.contains("unknown field `display_name`") || text.contains("unknown field"),
            "expected unknown field rejection, got: {text}"
        );
    }

    // 3. POST /polls with legacy `display_name`
    {
        let mut poll_json: serde_json::Value = serde_json::from_str(&signed_poll_body(
            &signing_key,
            "my-site",
            "page-1",
            "idemp-legacy-poll",
            "What?",
            &[("a", "Option A"), ("b", "Option B")],
            PollSemanticKind::Disclosed,
            1,
            None,
            None,
            &challenge.prefix,
            &challenge_response,
        ))
        .unwrap();
        poll_json["display_name"] = serde_json::json!("Legacy Eve");
        let body = poll_json.to_string();

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/polls",
                &[("idempotency-key", "idemp-legacy-poll")],
                &body.to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let text = body_text(res).await;
        assert!(
            text.contains("unknown field `display_name`") || text.contains("unknown field"),
            "expected unknown field rejection, got: {text}"
        );
    }

    // 4. PATCH /comments/{id} with legacy `display_name`
    {
        let body = serde_json::json!({
            "content": "Edited content",
            "display_name": "Legacy Eve",
            "author_public_key": public_key,
            "author_signature": "fake-sig",
            "challenge_response": challenge_response,
        });

        let res = router
            .clone()
            .oneshot(request(
                Method::PATCH,
                "/api/v1/sites/my-site/pages/page-1/comments/$existing-comment:hs",
                &[("idempotency-key", "idemp-legacy-patch")],
                &body.to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let text = body_text(res).await;
        assert!(
            text.contains("unknown field `display_name`") || text.contains("unknown field"),
            "expected unknown field rejection, got: {text}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Content operations never mutate author Matrix profile
// ---------------------------------------------------------------------------

#[tokio::test]
async fn content_creation_and_reconciliation_never_mutates_author_profile() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_and_store("creation-no-profile", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[2u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // Setup: Author explicitly sets profile display name to "Alice"
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let op_id = "op-set-name-alice";
        let sig_msg = set_display_name_signature_message("my-site", op_id, "Alice");
        let signature = sign(&signing_key, &sig_msg);

        let req_body = serde_json::json!({
            "display_name": "Alice",
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
        assert_eq!(body.status, ProfileOperationStatus::Completed);
        assert_eq!(body.value.as_deref(), Some("Alice"));
    }

    // Author profile is now "Alice". Verify driver received exactly 1 call to set_display_name.
    assert_eq!(driver.set_display_name_calls.lock().await.len(), 1);
    assert_eq!(driver.clear_display_name_calls.lock().await.len(), 0);

    let deps = reconciler_deps(store.clone(), driver.clone());
    let posts_pass = PostsPass::new(
        deps.clone(),
        PassConfig {
            name: "posts",
            interval: Duration::from_secs(5),
            wakeup: Arc::new(tokio::sync::Notify::new()),
        },
    );

    // 1. Post a comment without `display_name`
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let message = post_signature_message(
            "my-site",
            "page-1",
            "First comment",
            None,
            None,
            &challenge.prefix,
        );
        let signature = sign(&signing_key, &message);
        let body = serde_json::json!({
            "content": "First comment",
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/comments",
                &[("idempotency-key", "comment-key-1")],
                &body.to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::ACCEPTED);

        // Run reconciler posts pass
        let processed = posts_pass.run().await.expect("reconcile posts");
        assert_eq!(processed, 1);

        // Verify driver received the message write
        let posted = driver.posted_messages.lock().await;
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].content, "First comment");
        assert_eq!(posted[0].author_public_key, public_key);

        // Verify driver received ZERO profile mutations
        assert_eq!(driver.set_display_name_calls.lock().await.len(), 1);
        assert_eq!(driver.clear_display_name_calls.lock().await.len(), 0);
    }

    // 2. Post a location without `display_name`
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let message = locate_signature_message(
            "my-site",
            "page-1",
            "geo:37.7749,-122.4194",
            None,
            None,
            &challenge.prefix,
        );
        let signature = sign(&signing_key, &message);
        let body = serde_json::json!({
            "geo_uri": "geo:37.7749,-122.4194",
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/location",
                &[("idempotency-key", "location-key-1")],
                &body.to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::ACCEPTED);

        // Run reconciler posts pass
        let processed = posts_pass.run().await.expect("reconcile posts");
        assert_eq!(processed, 1);

        // Verify driver received the location write
        let locations = driver.posted_locations.lock().await;
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].geo_uri, "geo:37.7749,-122.4194");
        assert_eq!(locations[0].author_public_key, public_key);

        // Verify driver received ZERO profile mutations
        assert_eq!(driver.set_display_name_calls.lock().await.len(), 1);
        assert_eq!(driver.clear_display_name_calls.lock().await.len(), 0);
    }

    // 3. Post a poll without `display_name`
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let body = signed_poll_body(
            &signing_key,
            "my-site",
            "page-1",
            "poll-key-1",
            "Favorite editor?",
            &[("1", "Vim"), ("2", "Emacs")],
            PollSemanticKind::Disclosed,
            1,
            None,
            None,
            &challenge.prefix,
            &challenge_response,
        );

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/polls",
                &[("idempotency-key", "poll-key-1")],
                &body.to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::ACCEPTED);

        // Run reconciler posts pass
        let processed = posts_pass.run().await.expect("reconcile posts");
        assert_eq!(processed, 1);

        // Verify driver received the poll write
        let polls = driver.polls.lock().await;
        assert_eq!(polls.len(), 1);
        assert_eq!(polls[0].question, "Favorite editor?");

        // Verify driver received ZERO profile mutations
        assert_eq!(driver.set_display_name_calls.lock().await.len(), 1);
        assert_eq!(driver.clear_display_name_calls.lock().await.len(), 0);
    }

    // 4. Verify authoritative profile remains "Alice" throughout all content creations
    {
        let get_uri =
            format!("/api/v1/sites/my-site/visitors/profile?author_public_key={public_key}");
        let get_res = router
            .clone()
            .oneshot(request(Method::GET, &get_uri, &[], "{}"))
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::OK);
        let prof: serde_json::Value = body_json(get_res).await;
        assert_eq!(prof["display_name"], "Alice");
    }
}

// ---------------------------------------------------------------------------
// 3. Comment editing never reverts or mutates profile
// ---------------------------------------------------------------------------

#[tokio::test]
async fn comment_editing_never_mutates_profile_or_reverts_author_display_name() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_and_store("editing-no-revert", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[3u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    // Register the comment room so the registry knows the room
    store
        .register_room(
            "!room-my-site-blog-1:hs",
            &SiteId::from("my-site"),
            &PageSlug::from("blog-1"),
        )
        .await
        .unwrap();

    // Step 1: Author sets profile display name to "Alice"
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let op_id = "op-set-name-1";
        let sig_msg = set_display_name_signature_message("my-site", op_id, "Alice");
        let signature = sign(&signing_key, &sig_msg);

        let req_body = serde_json::json!({
            "display_name": "Alice",
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
    }

    // Step 2: Seed comment created while author's name was "Alice".
    // Stored message has author_display_name: Some("Alice") as captured historically.
    let comment_id = "$original-event-1:hs";
    store
        .save_message(&Message {
            event_id: comment_id.to_string(),
            site_id: "my-site".to_string(),
            page_slug: "blog-1".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Visitor,
                display_name: Some("Alice".to_string()),
                avatar_url: None,
                public_key: Some(public_key.clone()),
                mxid: None,
            },
            content: Content::Text(TextContent {
                body: "Initial text by Alice".to_string(),
                formatted_body: None,
                style: TextStyle::Normal,
            }),
            timestamp: chrono::Utc::now(),
            edited_at: None,
            reply_to: None,
            thread_root: None,
            submission_id: Some(101),
            status: MessageStatus::Active,
            redacted_at: None,
            redacted_by: None,
            reactions: vec![],
            thread_summary: None,
            room_id: "!room-my-site-blog-1:hs".to_string(),
            sender_mxid: "@_cumments_visitor:hs".to_string(),
            matrix_event_type: "m.room.message".to_string(),
            raw_content: serde_json::json!({}),
        })
        .await
        .unwrap();

    // Step 3: Author updates profile display name to "Bob"
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let op_id = "op-set-name-2";
        let sig_msg = set_display_name_signature_message("my-site", op_id, "Bob");
        let signature = sign(&signing_key, &sig_msg);

        let req_body = serde_json::json!({
            "display_name": "Bob",
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
    }

    // Verify author's current profile is now "Bob"
    {
        let get_uri =
            format!("/api/v1/sites/my-site/visitors/profile?author_public_key={public_key}");
        let get_res = router
            .clone()
            .oneshot(request(Method::GET, &get_uri, &[], "{}"))
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::OK);
        let prof: serde_json::Value = body_json(get_res).await;
        assert_eq!(prof["display_name"], "Bob");
    }

    // Snapshot driver profile calls before editing the comment
    let set_calls_before = driver.set_display_name_calls.lock().await.len();
    let clear_calls_before = driver.clear_display_name_calls.lock().await.len();
    assert_eq!(set_calls_before, 2); // "Alice" then "Bob"

    // Step 4: Author edits the comment created earlier
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let new_text = "Updated text by Bob";

        let sig_msg = signature_message(&[
            Some("PATCH"),
            Some("my-site"),
            Some("blog-1"),
            Some(comment_id),
            Some(new_text),
            Some(&challenge.prefix),
            Some("1"),
        ]);
        let signature = sign(&signing_key, &sig_msg);

        let req_body = serde_json::json!({
            "content": new_text,
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });

        let patch_uri = format!("/api/v1/sites/my-site/pages/blog-1/comments/{comment_id}");
        let res = router
            .clone()
            .oneshot(request(
                Method::PATCH,
                &patch_uri,
                &[("idempotency-key", "edit-key-100")],
                &req_body.to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::ACCEPTED);

        // Step 5: Run reconciler UpdatesPass
        let deps = reconciler_deps(store.clone(), driver.clone());
        let updates_pass = UpdatesPass::new(
            deps.clone(),
            PassConfig {
                name: "updates",
                interval: Duration::from_secs(5),
                wakeup: Arc::new(tokio::sync::Notify::new()),
            },
        );

        let processed = updates_pass.run().await.expect("reconcile updates");
        assert_eq!(processed, 1);

        // Verify driver received the update_message
        let updates = driver.updated_messages.lock().await;
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].new_content, new_text);
        assert_eq!(updates[0].event_id, comment_id);
    }

    // Step 6: Verify driver received ZERO profile mutations during the edit
    let set_calls_after = driver.set_display_name_calls.lock().await.len();
    let clear_calls_after = driver.clear_display_name_calls.lock().await.len();
    assert_eq!(
        set_calls_after, set_calls_before,
        "editing a comment must NEVER call set_display_name on the Matrix driver"
    );
    assert_eq!(
        clear_calls_after, clear_calls_before,
        "editing a comment must NEVER call clear_display_name on the Matrix driver"
    );

    // Step 7: Verify author's current profile remains "Bob" (NO reversion to "Alice"!)
    {
        let get_uri =
            format!("/api/v1/sites/my-site/visitors/profile?author_public_key={public_key}");
        let get_res = router
            .clone()
            .oneshot(request(Method::GET, &get_uri, &[], "{}"))
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::OK);
        let prof: serde_json::Value = body_json(get_res).await;
        assert_eq!(
            prof["display_name"], "Bob",
            "author profile must remain Bob and not revert to old comment snapshot Alice"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. Visitor without profile posting content leaves profile completely empty
// ---------------------------------------------------------------------------

#[tokio::test]
async fn visitor_without_profile_posting_content_leaves_profile_empty() {
    let driver = Arc::new(TestDriver::new());
    let (state, store) = test_state_and_store("no-profile-empty", driver.clone()).await;
    store
        .register_site("my-site", &token_hash("claim"), false)
        .await
        .unwrap();

    let router = cumments_api::build_router(state.clone());
    let signing_key = SigningKey::from_bytes(&[4u8; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let deps = reconciler_deps(store.clone(), driver.clone());
    let posts_pass = PostsPass::new(
        deps.clone(),
        PassConfig {
            name: "posts",
            interval: Duration::from_secs(5),
            wakeup: Arc::new(tokio::sync::Notify::new()),
        },
    );

    // 1. Post a comment without having any profile set
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let message = post_signature_message(
            "my-site",
            "page-1",
            "Anonymous comment",
            None,
            None,
            &challenge.prefix,
        );
        let signature = sign(&signing_key, &message);
        let body = serde_json::json!({
            "content": "Anonymous comment",
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/comments",
                &[("idempotency-key", "anon-comment-1")],
                &body.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);

        let processed = posts_pass.run().await.expect("reconcile posts");
        assert_eq!(processed, 1);
    }

    // 2. Post a location without having any profile set
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let message = locate_signature_message(
            "my-site",
            "page-1",
            "geo:51.5074,-0.1278",
            None,
            None,
            &challenge.prefix,
        );
        let signature = sign(&signing_key, &message);
        let body = serde_json::json!({
            "geo_uri": "geo:51.5074,-0.1278",
            "author_public_key": public_key,
            "author_signature": signature,
            "challenge_response": challenge_response,
        });

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/location",
                &[("idempotency-key", "anon-loc-1")],
                &body.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);

        let processed = posts_pass.run().await.expect("reconcile posts");
        assert_eq!(processed, 1);
    }

    // 3. Post a poll without having any profile set
    {
        let challenge = state.pow.generate_challenge();
        let challenge_response = solve_pow(&challenge);
        let body = signed_poll_body(
            &signing_key,
            "my-site",
            "page-1",
            "anon-poll-1",
            "Color?",
            &[("r", "Red"), ("b", "Blue")],
            PollSemanticKind::Disclosed,
            1,
            None,
            None,
            &challenge.prefix,
            &challenge_response,
        );

        let res = router
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/v1/sites/my-site/pages/page-1/polls",
                &[("idempotency-key", "anon-poll-1")],
                &body.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);

        let processed = posts_pass.run().await.expect("reconcile posts");
        assert_eq!(processed, 1);
    }

    // 4. Assert driver received writes for content
    assert_eq!(driver.posted_messages.lock().await.len(), 1);
    assert_eq!(driver.posted_locations.lock().await.len(), 1);
    assert_eq!(driver.polls.lock().await.len(), 1);

    // 5. Assert driver received ZERO profile mutations
    assert_eq!(
        driver.set_display_name_calls.lock().await.len(),
        0,
        "content creation must never call set_display_name"
    );
    assert_eq!(
        driver.clear_display_name_calls.lock().await.len(),
        0,
        "content creation must never call clear_display_name"
    );

    // 6. Assert GET /profile returns null display_name and null avatar
    {
        let get_uri =
            format!("/api/v1/sites/my-site/visitors/profile?author_public_key={public_key}");
        let get_res = router
            .clone()
            .oneshot(request(Method::GET, &get_uri, &[], "{}"))
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::OK);
        let prof: serde_json::Value = body_json(get_res).await;
        assert!(prof["display_name"].is_null());
        assert!(prof["avatar"].is_null());
        assert!(prof["avatar_url"].is_null());
    }
}

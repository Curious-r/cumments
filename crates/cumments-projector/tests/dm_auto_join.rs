use chrono::{Duration, Utc};
use cumments_core::models::{Content, TextContent, TextStyle};
use cumments_core::{
    governance::{NewRoleClaim, SITE_ADMIN_LEVEL},
    ports::{RoleClaimStore, SiteAuthStore},
};
use cumments_projector::{
    event_processor::{EventProcessor, EventProcessorDeps},
    parsed::{ParsedRoomMessage, ParsedRoomState},
};
use cumments_store::DbStore;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::{Notify, broadcast};
mod common;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-dm-auto-join-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

fn invite_event(room_id: &str, sender: &str) -> ParsedRoomState {
    ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$invite".to_string(),
        sender: sender.to_string(),
        event_type: "m.room.member".to_string(),
        state_key: "@_cumments_bot:hs".to_string(),
        origin_server_ts: 1,
        content: serde_json::json!({ "membership": "invite" }),
    }
}

fn invite_event_with_target(
    room_id: &str,
    sender: &str,
    target: &str,
    membership: &str,
) -> ParsedRoomState {
    ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: "$invite".to_string(),
        sender: sender.to_string(),
        event_type: "m.room.member".to_string(),
        state_key: target.to_string(),
        origin_server_ts: 1,
        content: serde_json::json!({ "membership": membership }),
    }
}

fn processor(store: Arc<DbStore>, driver: Arc<common::TestDriver>) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
        sticker_pack_store: store.clone(),
        projection_repair_store: store.clone(),
        role_claim_store: store.clone(),
        submission_store: store.clone(),
        audit_store: store.clone(),
        site_auth_store: store.clone(),
        site_auth_policy: std::sync::Arc::new(cumments_core::site_auth::SiteAuthPolicy {
            verification: cumments_core::site_auth::SiteVerificationPolicy::Optional,
            sites: Default::default(),
        }),
        site_service: std::sync::Arc::new(cumments_core::site_service::SiteService::new(
            store.clone() as std::sync::Arc<dyn cumments_core::ports::SiteStore>,
        )),
        driver: Some(driver),
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: Arc::new(Notify::new()),
        server_name: Some("hs".to_string()),
    })
}

fn command_message(sender: &str, body: &str) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: "!dm:hs".to_string(),
        event_id: "$msg".to_string(),
        event_type: "m.room.message".to_string(),
        sender: sender.to_string(),
        content: Content::Text(TextContent {
            body: body.to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        author_public_key: None,
        author_signature: None,
        author_challenge: None,
        is_virtual_user_sender: false,
        submission_id: None,
        reply_to: None,
        thread_root: None,
        origin_server_ts: 1,
        relates_to: None,
        room_identity: None,
        raw_content: serde_json::json!({}),
    }
}

// ── Self-service admission ────────────────────────────────────────

#[tokio::test]
async fn self_service_local_inviter_joins() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("local-join"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::new());
    processor(store, driver.clone())
        .process_room_state(invite_event("!dm-local:hs", "@alice:hs"))
        .await
        .expect("process");
    assert_eq!(*driver.joined.lock().await, vec!["!dm-local:hs"]);
}

#[tokio::test]
async fn self_service_federated_inviter_joins() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("federated-join"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::new());
    processor(store, driver.clone())
        .process_room_state(invite_event("!dm-fed:hs", "@alice:matrix.org"))
        .await
        .expect("process");
    assert_eq!(*driver.joined.lock().await, vec!["!dm-fed:hs"]);
}

#[tokio::test]
async fn self_service_federated_example_net_joins() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("example-net"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::new());
    processor(store, driver.clone())
        .process_room_state(invite_event("!dm-en:hs", "@bob:example.net"))
        .await
        .expect("process");
    assert_eq!(*driver.joined.lock().await, vec!["!dm-en:hs"]);
}

#[tokio::test]
async fn bootstrap_rejects_as_managed_inviter() {
    let store = Arc::new(DbStore::connect(&test_db_url("managed")).await.expect("db"));
    let driver = Arc::new(common::TestDriver::new());
    processor(store, driver.clone())
        .process_room_state(invite_event("!dm:hs", "@_cumments_bot:hs"))
        .await
        .expect("process");
    assert!(
        driver.joined.lock().await.is_empty(),
        "managed bot itself must not trigger bootstrap"
    );

    let store2 = Arc::new(
        DbStore::connect(&test_db_url("managed2"))
            .await
            .expect("db"),
    );
    let driver2 = Arc::new(common::TestDriver::new());
    processor(store2, driver2.clone())
        .process_room_state(invite_event("!dm2:hs", "@_cumments_visitor:hs"))
        .await
        .expect("process");
    assert!(
        driver2.joined.lock().await.is_empty(),
        "AS-managed visitor must be rejected"
    );
}

#[tokio::test]
async fn bootstrap_rejects_malformed_mxid() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("malformed"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::new());
    processor(store.clone(), driver.clone())
        .process_room_state(invite_event("!dm:hs", "not-a-mxid"))
        .await
        .expect("process");
    assert!(driver.joined.lock().await.is_empty());

    let driver2 = Arc::new(common::TestDriver::new());
    processor(store, driver2.clone())
        .process_room_state(invite_event("!dm2:hs", "@alice:"))
        .await
        .expect("process");
    assert!(driver2.joined.lock().await.is_empty());
}

#[tokio::test]
async fn non_bot_target_ignored() {
    let store = Arc::new(DbStore::connect(&test_db_url("non-bot")).await.expect("db"));
    let driver = Arc::new(common::TestDriver::new());
    processor(store, driver.clone())
        .process_room_state(invite_event_with_target(
            "!dm:hs",
            "@alice:hs",
            "@other:hs",
            "invite",
        ))
        .await
        .expect("process");
    assert!(driver.joined.lock().await.is_empty());
}

#[tokio::test]
async fn non_invite_membership_ignored() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("non-invite"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::new());
    for membership in ["join", "leave", "ban", "knock"] {
        let store = store.clone();
        let driver = driver.clone();
        let event =
            invite_event_with_target("!dm:hs", "@alice:hs", "@_cumments_bot:hs", membership);
        processor(store, driver.clone())
            .process_room_state(event)
            .await
            .expect("process");
    }
    assert!(driver.joined.lock().await.is_empty());
}

// ── Existing claim path preservation ──────────────────────────────

#[tokio::test]
async fn pending_claim_local_joins_claim_path() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("claim-local"))
            .await
            .expect("db"),
    );
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "s".to_string(),
            room_id: String::new(),
            user_id: "@owner:hs".to_string(),
            level: SITE_ADMIN_LEVEL,
            token_hash: "h".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        })
        .await
        .expect("upsert");
    let driver = Arc::new(common::TestDriver::new());
    processor(store.clone(), driver.clone())
        .process_room_state(invite_event("!dm-claim:hs", "@owner:hs"))
        .await
        .expect("process");
    assert_eq!(*driver.joined.lock().await, vec!["!dm-claim:hs"]);
    assert!(
        store.claim_dm_room_exists("!dm-claim:hs").await.unwrap(),
        "claim path must set claim_dm_room"
    );
}

#[tokio::test]
async fn pending_claim_federated_still_joins() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("claim-fed"))
            .await
            .expect("db"),
    );
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "s".to_string(),
            room_id: String::new(),
            user_id: "@alice:matrix.org".to_string(),
            level: SITE_ADMIN_LEVEL,
            token_hash: "h".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        })
        .await
        .expect("upsert");
    let driver = Arc::new(common::TestDriver::new());
    processor(store.clone(), driver.clone())
        .process_room_state(invite_event("!dm-fed-claim:hs", "@alice:matrix.org"))
        .await
        .expect("process");
    assert_eq!(*driver.joined.lock().await, vec!["!dm-fed-claim:hs"]);
    assert!(
        store
            .claim_dm_room_exists("!dm-fed-claim:hs")
            .await
            .unwrap()
    );
}

// ── Governance isolation ──────────────────────────────────────────

#[tokio::test]
async fn bootstrap_join_does_not_create_claim_dm_room() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("no-claim-room"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::new());
    processor(store.clone(), driver.clone())
        .process_room_state(invite_event("!dm-boot:hs", "@alice:matrix.org"))
        .await
        .expect("process");
    assert_eq!(*driver.joined.lock().await, vec!["!dm-boot:hs"]);
    assert!(
        !store.claim_dm_room_exists("!dm-boot:hs").await.unwrap(),
        "bootstrap must NOT set claim_dm_room"
    );
}

#[tokio::test]
async fn group_room_governance_blocked() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("group-block"))
            .await
            .expect("db"),
    );
    // Simulate group: driver reports 3 joined members (bot + alice + bob)
    let driver = Arc::new(common::TestDriver::with_joined_members(vec![
        "@_cumments_bot:hs".to_string(),
        "@alice:matrix.org".to_string(),
        "@bob:hs".to_string(),
    ]));
    let p = processor(store.clone(), driver);
    // Invite via bootstrap path first to get joined (member save irrelevant)
    // Then attempt sites register in group room -> must be blocked by is_private_channel
    let joined = p
        .process_bot_command(&ParsedRoomMessage {
            room_id: "!group:hs".to_string(),
            event_id: "$g".to_string(),
            event_type: "m.room.message".to_string(),
            sender: "@alice:matrix.org".to_string(),
            content: Content::Text(TextContent {
                body: "!cumments sites register group-site".to_string(),
                formatted_body: None,
                style: TextStyle::Normal,
            }),
            author_public_key: None,
            author_signature: None,
            author_challenge: None,
            is_virtual_user_sender: false,
            submission_id: None,
            reply_to: None,
            thread_root: None,
            origin_server_ts: 1,
            relates_to: None,
            room_identity: None,
            raw_content: serde_json::json!({}),
        })
        .await
        .expect("process");
    assert!(joined, "command must be consumed even when not private");
    assert!(
        store
            .get_site_auth("group-site")
            .await
            .expect("get")
            .is_none(),
        "group room must not create site"
    );
}

#[tokio::test]
async fn membership_alone_causes_no_governance_mutation() {
    let store = Arc::new(DbStore::connect(&test_db_url("no-mut")).await.expect("db"));
    let driver = Arc::new(common::TestDriver::new());
    processor(store.clone(), driver.clone())
        .process_room_state(invite_event("!dm:hs", "@alice:hs"))
        .await
        .expect("process");
    assert!(store.get_site_auth("any").await.expect("get").is_none());
    assert!(
        store
            .list_applied_claims()
            .await
            .expect("claims")
            .is_empty()
    );
}

// ── Self-service end-to-end (federated) ───────────────────────────

#[tokio::test]
async fn federated_self_service_end_to_end() {
    let store = Arc::new(DbStore::connect(&test_db_url("e2e-fed")).await.expect("db"));
    // After join, the next command sees joined_members { alice, bot } len==2
    let driver = Arc::new(common::TestDriver::with_joined_members(vec![
        "@_cumments_bot:hs".to_string(),
        "@alice:matrix.org".to_string(),
    ]));
    let p = processor(store.clone(), driver.clone());
    // 1. invite
    p.process_room_state(invite_event("!dm:hs", "@alice:matrix.org"))
        .await
        .expect("invite");
    assert_eq!(*driver.joined.lock().await, vec!["!dm:hs"]);
    // 2. sites register
    let consumed = p
        .process_bot_command(&command_message(
            "@alice:matrix.org",
            "!cumments sites register curious",
        ))
        .await
        .expect("cmd");
    assert!(consumed);
    assert!(
        store
            .get_site_auth("curious")
            .await
            .expect("site")
            .is_some()
    );
    let applied = store.list_applied_claims().await.expect("applied");
    assert!(
        applied
            .iter()
            .any(|c| c.user_id == "@alice:matrix.org" && c.level == SITE_ADMIN_LEVEL)
    );
}

#[tokio::test]
async fn local_self_service_end_to_end() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("e2e-local"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::with_joined_members(vec![
        "@_cumments_bot:hs".to_string(),
        "@alice:hs".to_string(),
    ]));
    let p = processor(store.clone(), driver.clone());
    p.process_room_state(invite_event("!dm:hs", "@alice:hs"))
        .await
        .expect("invite");
    assert_eq!(*driver.joined.lock().await, vec!["!dm:hs"]);
    let consumed = p
        .process_bot_command(&command_message(
            "@alice:hs",
            "!cumments sites register localsite",
        ))
        .await
        .expect("cmd");
    assert!(consumed);
    assert!(
        store
            .get_site_auth("localsite")
            .await
            .expect("site")
            .is_some()
    );
}

// ── Rate limiter ──────────────────────────────────────────────────

#[tokio::test]
async fn rate_limiter_allows_five_then_denies_sixth() {
    let store = Arc::new(DbStore::connect(&test_db_url("rl-5")).await.expect("db"));
    let driver = Arc::new(common::TestDriver::new());
    let p = processor(store, driver.clone());
    for i in 0..5 {
        p.process_room_state(invite_event(&format!("!dm{i}:hs"), "@alice:hs"))
            .await
            .expect("invite");
    }
    assert_eq!(driver.joined.lock().await.len(), 5);
    // 6th within same 60s window -> rate-limited
    p.process_room_state(invite_event("!dm5:hs", "@alice:hs"))
        .await
        .expect("invite");
    assert_eq!(
        driver.joined.lock().await.len(),
        5,
        "6th must be join_rate_limited"
    );
}

#[tokio::test]
async fn rate_limiter_independent_buckets() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("rl-indep"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::new());
    let p = processor(store, driver.clone());
    for i in 0..5 {
        p.process_room_state(invite_event(&format!("!dm-a{i}:hs"), "@alice:hs"))
            .await
            .expect("invite");
    }
    // different inviter -> independent bucket
    p.process_room_state(invite_event("!dm-bob:hs", "@bob:hs"))
        .await
        .expect("invite");
    assert_eq!(driver.joined.lock().await.len(), 6);
    assert!(
        driver
            .joined
            .lock()
            .await
            .contains(&"!dm-bob:hs".to_string())
    );
}

#[tokio::test]
async fn rate_limiter_allows_after_expiry_via_allow_at() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("rl-expiry"))
            .await
            .expect("db"),
    );
    let driver = Arc::new(common::TestDriver::new());
    let p = processor(store, driver);
    let start = Instant::now();
    // Exercise the limiter directly via allow_at for deterministic time
    let limiter = p.invite_join_limiter_for_test();
    for _ in 0..5 {
        assert!(limiter.allow_at("@alice:hs", start));
    }
    assert!(
        !limiter.allow_at("@alice:hs", start + StdDuration::from_secs(30)),
        "6th at +30s must be denied"
    );
    assert!(
        limiter.allow_at("@alice:hs", start + StdDuration::from_secs(61)),
        "after 61s window expiry must allow"
    );
}

#[tokio::test]
async fn rate_limited_invite_does_not_query_pending_claims_or_join() {
    // pending claim exists, but limiter already exhausted -> no join even though claim would otherwise allow join
    let store = Arc::new(
        DbStore::connect(&test_db_url("rl-no-query"))
            .await
            .expect("db"),
    );
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "s".to_string(),
            room_id: String::new(),
            user_id: "@alice:hs".to_string(),
            level: SITE_ADMIN_LEVEL,
            token_hash: "h".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        })
        .await
        .expect("upsert");
    let driver = Arc::new(common::TestDriver::new());
    let p = processor(store.clone(), driver.clone());
    // Fill quota with 5 bootstrap invites (no claim needed, but we have a claim now)
    // To fill with the same inviter that has pending claim, we need to use that inviter
    for i in 0..5 {
        p.process_room_state(invite_event(&format!("!dm{i}:hs"), "@alice:hs"))
            .await
            .expect("invite");
    }
    assert_eq!(driver.joined.lock().await.len(), 5);
    // 6th invite from same inviter with pending claim must be rate-limited, not joined, even though pending_claims non-empty
    p.process_room_state(invite_event("!dm5:hs", "@alice:hs"))
        .await
        .expect("invite");
    assert_eq!(
        driver.joined.lock().await.len(),
        5,
        "rate-limited pending-claim invite must not join; proves limiter before DB"
    );
}

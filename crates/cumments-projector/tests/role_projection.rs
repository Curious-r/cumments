use cumments_core::governance::{NewRoleClaim, RoleEntry};
use cumments_core::models::{Content, PostSlug, SiteId, TextContent, TextStyle};
use cumments_core::ports::{GovernanceStore, RegistryStore, RoleClaimStore, SiteStore};
use cumments_core::site_auth::token_hash;
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::{ParsedRoomMessage, ParsedRoomState};
use cumments_store::DbStore;
mod common;
use std::sync::Arc;
use tokio::sync::{Notify, broadcast};

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-role-projection-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn state_event(room_id: &str, power_levels: serde_json::Value) -> ParsedRoomState {
    ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: format!("$pl:{}", room_id),
        sender: "@_cumments_bot:hs".to_string(),
        event_type: "m.room.power_levels".to_string(),
        state_key: String::new(),
        origin_server_ts: 1,
        content: power_levels,
    }
}

fn claim_message(sender: &str, body: &str) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: "!dm:hs".to_string(),
        event_id: "$dm:hs".to_string(),
        sender: sender.to_string(),
        content: Content::Text(TextContent {
            body: body.to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
        display_name: None,
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
        raw_content: serde_json::Value::Null,
    }
}

#[tokio::test]
async fn power_levels_project_site_and_room_roles() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("roles"))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("valid site id");
    let post_slug = PostSlug::new("hello".to_string()).expect("valid post slug");

    // The space and one comment room exist before the state events arrive.
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("site");
    store
        .register_room("!room:hs", &site_id, &post_slug)
        .await
        .expect("room");

    let (tx, _rx) = broadcast::channel(16);
    let projection_notify = Arc::new(Notify::new());
    let processor = EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
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
        driver: None,
        admin_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: projection_notify.clone(),
        server_name: Some("hs".to_string()),
    });

    // The Space's roles exclude the AS-managed sender.
    processor
        .process_room_state(state_event(
            "!space:hs",
            serde_json::json!({
                "users": {
                    "@owner:hs": 100,
                    "@co:hs": 75,
                    "@_cumments_bot:hs": 100,
                    "@plain-member:hs": 0,
                }
            }),
        ))
        .await
        .expect("project space roles");
    assert_eq!(
        store.list_site_roles("my-blog").await.expect("site roles"),
        vec![
            RoleEntry {
                user_id: "@co:hs".into(),
                level: 75
            },
            RoleEntry {
                user_id: "@owner:hs".into(),
                level: 100
            },
        ]
    );
    // Projecting a Space's power levels must wake the moderation sync.
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            projection_notify.notified()
        )
        .await
        .is_ok(),
        "space power-levels projection must notify the reconciler"
    );

    // A comment room projects its own (independent) moderator roster.
    processor
        .process_room_state(state_event(
            "!room:hs",
            serde_json::json!({
                "users": {
                    "@owner:hs": 100,
                    "@mod:hs": 50,
                    "@_cumments_bot:hs": 100,
                }
            }),
        ))
        .await
        .expect("project room roles");
    assert_eq!(
        store.list_room_roles("!room:hs").await.expect("room roles"),
        vec![
            RoleEntry {
                user_id: "@mod:hs".into(),
                level: 50
            },
            RoleEntry {
                user_id: "@owner:hs".into(),
                level: 100
            },
        ]
    );
}

#[tokio::test]
async fn claim_dm_activates_only_the_matching_token() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("claim-dm"))
            .await
            .expect("connect db"),
    );
    let (tx, _rx) = broadcast::channel(16);
    let projection_notify = Arc::new(Notify::new());
    let processor = EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
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
        driver: Some(Arc::new(common::TestDriver::with_joined_members(vec![
            "@_cumments_bot:hs".to_string(),
            "@alice:hs".to_string(),
        ]))),
        admin_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: projection_notify.clone(),
        server_name: Some("hs".to_string()),
    });

    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "my-blog".to_string(),
            room_id: String::new(),
            user_id: "@alice:hs".to_string(),
            level: 100,
            token_hash: token_hash("secret-token"),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(24),
        })
        .await
        .expect("create claim");

    let wrong = claim_message("@alice:hs", "cumments-claim:wrong-token");
    assert!(
        !processor
            .process_claim_dm(&wrong)
            .await
            .expect("wrong token"),
        "a wrong token must not activate the claim"
    );
    assert_eq!(
        store
            .pending_claims_for_user("@alice:hs")
            .await
            .expect("pending claims")
            .len(),
        1
    );

    let right = claim_message("@alice:hs", "cumments-claim:secret-token");
    assert!(
        processor
            .process_claim_dm(&right)
            .await
            .expect("right token"),
        "the matching token must activate the claim"
    );
    assert!(
        store
            .pending_claims_for_user("@alice:hs")
            .await
            .expect("pending claims")
            .is_empty()
    );
    assert_eq!(
        store
            .activated_unapplied_claims()
            .await
            .expect("activated claims")
            .len(),
        1
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            projection_notify.notified(),
        )
        .await
        .is_ok(),
        "claim activation must wake the reconciler"
    );
}

#[tokio::test]
async fn claim_dm_requires_a_verified_private_channel() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("claim-dm-private"))
            .await
            .expect("connect db"),
    );
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "my-blog".to_string(),
            room_id: String::new(),
            user_id: "@bob:hs".to_string(),
            level: 100,
            token_hash: token_hash("secret-token"),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(24),
        })
        .await
        .expect("create claim");

    let (tx, _rx) = broadcast::channel(16);
    // Three joined members: not a private channel, so even a valid token
    // must not activate the claim.
    let processor = EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
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
        driver: Some(Arc::new(common::TestDriver::with_joined_members(vec![
            "@_cumments_bot:hs".to_string(),
            "@alice:hs".to_string(),
            "@bob:hs".to_string(),
        ]))),
        admin_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: Arc::new(Notify::new()),
        server_name: Some("hs".to_string()),
    });

    let message = claim_message("@bob:hs", "cumments-claim:secret-token");
    assert!(
        !processor
            .process_claim_dm(&message)
            .await
            .expect("process claim"),
        "claims must not activate outside a verified private channel"
    );
    assert_eq!(
        store
            .pending_claims_for_user("@bob:hs")
            .await
            .expect("pending claims")
            .len(),
        1
    );
}

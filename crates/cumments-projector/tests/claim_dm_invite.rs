use chrono::{Duration, Utc};
use cumments_core::{
    governance::{NewRoleClaim, SITE_ADMIN_LEVEL},
    ports::RoleClaimStore,
};
use cumments_projector::{
    event_processor::{EventProcessor, EventProcessorDeps},
    parsed::ParsedRoomState,
};
use cumments_store::DbStore;
use std::sync::Arc;
use tokio::sync::{Notify, broadcast};
mod common;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-claim-dm-invite-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
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

#[tokio::test]
async fn bot_joins_dm_only_when_inviter_has_a_pending_claim() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("with-claim"))
            .await
            .expect("connect db"),
    );
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "my-blog".to_string(),
            room_id: String::new(),
            user_id: "@owner:hs".to_string(),
            level: SITE_ADMIN_LEVEL,
            token_hash: "hash".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        })
        .await
        .expect("upsert claim");

    let driver = Arc::new(common::TestDriver::new());
    processor(store.clone(), driver.clone())
        .process_room_state(invite_event("!dm:hs", "@owner:hs"))
        .await
        .expect("process invite");
    assert_eq!(*driver.joined.lock().await, vec!["!dm:hs"]);
    assert!(store.claim_dm_room_exists("!dm:hs").await.unwrap());

    let stranger_store = Arc::new(
        DbStore::connect(&test_db_url("without-claim"))
            .await
            .expect("connect db"),
    );
    let stranger_driver = Arc::new(common::TestDriver::new());
    processor(stranger_store.clone(), stranger_driver.clone())
        .process_room_state(invite_event("!other-dm:hs", "@stranger:hs"))
        .await
        .expect("process invite");
    // Self-service bootstrap now allows normal users without pending claim to auto-join
    // (public/federated service). Verify bootstrap join occurs without claim-DM bookkeeping.
    assert_eq!(*stranger_driver.joined.lock().await, vec!["!other-dm:hs"]);
    assert!(
        !stranger_store
            .claim_dm_room_exists("!other-dm:hs")
            .await
            .unwrap()
    );

    // AS-managed users must still be rejected even without pending claim
    let managed_store = Arc::new(
        DbStore::connect(&test_db_url("managed-no-join"))
            .await
            .expect("connect db"),
    );
    let managed_driver = Arc::new(common::TestDriver::new());
    processor(managed_store, managed_driver.clone())
        .process_room_state(invite_event("!managed-dm:hs", "@_cumments_bot:hs"))
        .await
        .expect("process invite");
    assert!(managed_driver.joined.lock().await.is_empty());
}

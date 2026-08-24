use cumments_core::{
    audit::CommandAuditStatus,
    governance::{MANAGER_LEVEL, MODERATOR_LEVEL, NewRoleClaim, RoleEntry, SITE_ADMIN_LEVEL},
    models::{Content, PageSlug, RoomStatus, SiteId, TextContent, TextStyle},
    ports::{
        CommandAuditStore, GovernanceStore, RegistryStore, RoleClaimStore, SiteAuthStore, SiteStore,
    },
    site_auth::{
        SiteAuthMode, SiteAuthPolicy, SiteLifecycle, SitePolicyEntry, SiteVerificationPolicy,
        token_hash,
    },
};
use cumments_projector::{
    event_processor::{EventProcessor, EventProcessorDeps},
    parsed::ParsedRoomMessage,
};
use cumments_store::DbStore;
mod common;
use std::sync::Arc;
use tokio::sync::{Notify, broadcast};

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-bot-commands-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

fn command_message(sender: &str, body: &str) -> ParsedRoomMessage {
    ParsedRoomMessage {
        room_id: "!dm:hs".to_string(),
        event_id: "$cmd".to_string(),
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
        raw_content: serde_json::Value::Null,
    }
}

fn processor(
    store: Arc<DbStore>,
    members: Vec<String>,
    operator_mxids: Vec<String>,
) -> EventProcessor {
    processor_with(store, members, operator_mxids, None, common::test_policy())
}

fn processor_with(
    store: Arc<DbStore>,
    members: Vec<String>,
    operator_mxids: Vec<String>,
    backfill_tx: Option<tokio::sync::mpsc::Sender<cumments_projector::backfill::BackfillRequest>>,
    policy: std::sync::Arc<cumments_core::site_auth::SiteAuthPolicy>,
) -> EventProcessor {
    processor_with_driver(
        store,
        Arc::new(common::TestDriver::with_joined_members(members)),
        operator_mxids,
        backfill_tx,
        policy,
    )
}

fn processor_with_driver(
    store: Arc<DbStore>,
    driver: Arc<common::TestDriver>,
    operator_mxids: Vec<String>,
    backfill_tx: Option<tokio::sync::mpsc::Sender<cumments_projector::backfill::BackfillRequest>>,
    policy: std::sync::Arc<cumments_core::site_auth::SiteAuthPolicy>,
) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
        sticker_pack_store: store.clone(),
        role_claim_store: store.clone(),
        submission_store: store.clone(),
        audit_store: store.clone(),
        site_auth_store: store.clone(),
        site_auth_policy: policy,
        site_service: Arc::new(cumments_core::site_service::SiteService::new(
            store.clone() as Arc<dyn cumments_core::ports::SiteStore>
        )),
        driver: Some(driver),
        operator_mxids,
        backfill_tx,
        event_bus: tx,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: Arc::new(Notify::new()),
        server_name: Some("hs".to_string()),
    })
}

fn private_members(sender: &str) -> Vec<String> {
    vec!["@_cumments_bot:hs".to_string(), sender.to_string()]
}

#[tokio::test]
async fn unknown_command_replies_with_help() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("help"))
            .await
            .expect("connect db"),
    );
    let p = processor(store.clone(), private_members("@alice:hs"), Vec::new());
    assert!(
        p.process_bot_command(&command_message("@alice:hs", "!cumments nope"))
            .await
            .expect("process"),
        "command must be consumed"
    );
    let audit = store
        .list_command_audit(Some("@alice:hs"), 10)
        .await
        .expect("audit");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].status, CommandAuditStatus::Invalid);
}

#[tokio::test]
async fn per_sender_limit_counts_the_first_command_and_silently_consumes_denials() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("per-sender-limit"))
            .await
            .expect("connect db"),
    );
    let sender = "@alice:hs";
    let driver = Arc::new(common::TestDriver::with_joined_members(private_members(
        sender,
    )));
    let p = processor_with_driver(
        store.clone(),
        driver.clone(),
        Vec::new(),
        None,
        common::test_policy(),
    );

    for _ in 0..10 {
        assert!(
            p.process_bot_command(&command_message(sender, "!cumments"))
                .await
                .expect("process")
        );
    }

    assert!(
        p.process_bot_command(&command_message(sender, "!cumments"))
            .await
            .expect("denial must be consumed")
    );
    let audits = store
        .list_command_audit(Some(sender), 20)
        .await
        .expect("audit");
    assert_eq!(audits.len(), 10);
    assert_eq!(driver.replies.lock().await.len(), 10);
}

#[tokio::test]
async fn exhausted_ingress_budget_does_not_query_membership_or_audit() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("ingress-limit"))
            .await
            .expect("connect db"),
    );
    let sender = "@alice:hs";
    let driver = Arc::new(common::TestDriver::with_joined_members(private_members(
        sender,
    )));
    let p = processor_with_driver(
        store.clone(),
        driver.clone(),
        Vec::new(),
        None,
        common::test_policy(),
    );

    for _ in 0..600 {
        assert!(
            p.process_bot_command(&command_message("@outside:hs", "!cumments"))
                .await
                .expect("prefix event")
        );
    }

    let queries_before = driver.joined_member_queries.lock().await.len();
    assert_eq!(queries_before, 600);
    assert!(
        p.process_bot_command(&command_message(sender, "!cumments"))
            .await
            .expect("throttled event must be consumed")
    );
    assert_eq!(
        driver.joined_member_queries.lock().await.len(),
        queries_before
    );
    assert!(driver.replies.lock().await.is_empty());
    assert!(
        store
            .list_command_audit(Some(sender), 10)
            .await
            .expect("audit")
            .is_empty()
    );
}

#[tokio::test]
async fn operator_sites_list_works_and_unknown_user_is_denied() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("sites-list"))
            .await
            .expect("connect db"),
    );
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");

    let p = processor(
        store.clone(),
        private_members(":hs"),
        vec![":hs".to_string()],
    );
    assert!(
        p.process_bot_command(&command_message(":hs", "!cumments sites list"))
            .await
            .expect("process"),
        "operator command consumed"
    );

    let denied = processor(store.clone(), private_members("@stranger:hs"), Vec::new());
    assert!(
        denied
            .process_bot_command(&command_message("@stranger:hs", "!cumments sites list"))
            .await
            .expect("process"),
        "denied command consumed"
    );
    let audit = store
        .list_command_audit(Some("@stranger:hs"), 10)
        .await
        .expect("audit");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].status, CommandAuditStatus::Denied);
}

#[tokio::test]
async fn admin_can_create_manager_claim_and_retire_with_confirm() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("admin"))
            .await
            .expect("connect db"),
    );
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    store
        .replace_site_roles(
            "my-blog",
            &[RoleEntry {
                user_id: "@alice:hs".into(),
                level: SITE_ADMIN_LEVEL,
            }],
        )
        .await
        .expect("project admin");
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("attach space");

    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@alice:hs")).with_power_levels(
            "!space:hs",
            serde_json::json!({
                "users": { "@alice:hs": 100 },
                "events": { "m.room.power_levels": 100 },
                "state_default": 50,
            }),
        ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver,
        Vec::new(),
        None,
        common::test_policy(),
    );
    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog manager add @bob:hs",
        ))
        .await
        .expect("process")
    );
    assert_eq!(
        store
            .pending_claims_for_user("@bob:hs")
            .await
            .expect("pending")
            .len(),
        1
    );

    // A single owned site makes `site status` unambiguous without `use`.
    assert!(
        p.process_bot_command(&command_message("@alice:hs", "!cumments site status"))
            .await
            .expect("process")
    );

    // Retire asks for confirmation first, then succeeds with --confirm.
    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog retire",
        ))
        .await
        .expect("process")
    );
    assert_eq!(
        store
            .get_site_auth("my-blog")
            .await
            .expect("site")
            .expect("exists")
            .lifecycle,
        SiteLifecycle::Active
    );
    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog retire --confirm",
        ))
        .await
        .expect("process")
    );
    assert_eq!(
        store
            .get_site_auth("my-blog")
            .await
            .expect("site")
            .expect("exists")
            .lifecycle,
        SiteLifecycle::Retiring
    );
}

#[tokio::test]
async fn manager_can_appoint_room_moderator_from_room_power() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("manager-room-moderator"))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("attach space");
    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@manager:hs"))
            .with_power_levels(
                "!space:hs",
                serde_json::json!({
                    "users": { "@manager:hs": MANAGER_LEVEL },
                    "events": { "m.room.power_levels": 100 },
                    "state_default": 50,
                }),
            )
            .with_power_levels(
                "!room:hs",
                serde_json::json!({
                    "users": { "@manager:hs": MANAGER_LEVEL },
                    "events": { "m.room.power_levels": 75 },
                    "state_default": 50,
                }),
            ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver,
        Vec::new(),
        None,
        common::test_policy(),
    );
    assert!(
        p.process_bot_command(&command_message(
            "@manager:hs",
            "!cumments site my-blog page hello moderator add @mod:hs",
        ))
        .await
        .expect("process")
    );
    assert_eq!(
        store
            .pending_claims_for_user("@mod:hs")
            .await
            .expect("pending")
            .len(),
        1
    );
}

#[tokio::test]
async fn fifty_level_moderator_cannot_appoint_room_moderator() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("mod-cannot-appoint"))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("attach space");
    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@mod:hs")).with_power_levels(
            "!room:hs",
            serde_json::json!({
                "users": { "@mod:hs": MODERATOR_LEVEL },
                "events": { "m.room.power_levels": 75 },
                "state_default": 50,
            }),
        ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver,
        Vec::new(),
        None,
        common::test_policy(),
    );
    assert!(
        p.process_bot_command(&command_message(
            "@mod:hs",
            "!cumments site my-blog page hello moderator add @other:hs",
        ))
        .await
        .expect("process")
    );
    let audit = store
        .list_command_audit(Some("@mod:hs"), 10)
        .await
        .expect("audit");
    assert_eq!(audit[0].status, CommandAuditStatus::Denied);
}

#[tokio::test]
async fn manager_can_resign_with_confirm() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("manager-resign"))
            .await
            .expect("connect db"),
    );
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("attach space");
    store
        .replace_site_roles(
            "my-blog",
            &[RoleEntry {
                user_id: "@manager:hs".into(),
                level: MANAGER_LEVEL,
            }],
        )
        .await
        .expect("project manager");
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "my-blog".to_string(),
            room_id: String::new(),
            user_id: "@manager:hs".to_string(),
            level: MANAGER_LEVEL,
            token_hash: "hash".to_string(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        })
        .await
        .expect("claim");
    let claim = store
        .pending_claims_for_user("@manager:hs")
        .await
        .expect("pending")
        .remove(0);
    assert!(store.mark_claim_activated(claim.id).await.unwrap());
    let activated = store
        .activated_unapplied_claims()
        .await
        .expect("activated")
        .remove(0);
    store
        .mark_claim_applied(activated.id)
        .await
        .expect("applied");

    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@manager:hs")).with_power_levels(
            "!space:hs",
            serde_json::json!({
                "users": { "@manager:hs": MANAGER_LEVEL },
                "events": { "m.room.power_levels": 100 },
                "state_default": 50,
            }),
        ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver.clone(),
        Vec::new(),
        None,
        common::test_policy(),
    );
    assert!(
        p.process_bot_command(&command_message(
            "@manager:hs",
            "!cumments site my-blog manager resign --confirm",
        ))
        .await
        .expect("process")
    );
    assert!(
        driver.power_levels.lock().await.get("!space:hs").unwrap()["users"]
            .get("@manager:hs")
            .is_none()
    );
    assert!(
        store
            .list_applied_claims()
            .await
            .expect("claims")
            .is_empty()
    );
}

#[tokio::test]
async fn moderator_can_resign_with_confirm() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("moderator-resign"))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    store
        .replace_room_roles(
            "!room:hs",
            &[RoleEntry {
                user_id: "@mod:hs".into(),
                level: MODERATOR_LEVEL,
            }],
        )
        .await
        .expect("project moderator");
    store
        .upsert_role_claim(&NewRoleClaim {
            site_id: "my-blog".to_string(),
            room_id: "!room:hs".to_string(),
            user_id: "@mod:hs".to_string(),
            level: MODERATOR_LEVEL,
            token_hash: "hash".to_string(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        })
        .await
        .expect("claim");
    let claim = store
        .pending_claims_for_user("@mod:hs")
        .await
        .expect("pending")
        .remove(0);
    assert!(store.mark_claim_activated(claim.id).await.unwrap());
    let activated = store
        .activated_unapplied_claims()
        .await
        .expect("activated")
        .remove(0);
    store
        .mark_claim_applied(activated.id)
        .await
        .expect("applied");

    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@mod:hs")).with_power_levels(
            "!room:hs",
            serde_json::json!({
                "users": { "@mod:hs": MODERATOR_LEVEL },
                "events": { "m.room.power_levels": 75 },
                "state_default": 50,
            }),
        ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver.clone(),
        Vec::new(),
        None,
        common::test_policy(),
    );
    assert!(
        p.process_bot_command(&command_message(
            "@mod:hs",
            "!cumments site my-blog page hello moderator resign --confirm",
        ))
        .await
        .expect("process")
    );
    assert!(
        driver.power_levels.lock().await.get("!room:hs").unwrap()["users"]
            .get("@mod:hs")
            .is_none()
    );
    assert!(
        store
            .list_applied_claims()
            .await
            .expect("claims")
            .is_empty()
    );
}

#[tokio::test]
async fn site_registration_is_public_self_service() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("register"))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(common::TestDriver::with_joined_members(private_members(
        "@alice:hs",
    )));
    let p = processor_with_driver(
        store.clone(),
        driver.clone(),
        Vec::new(),
        None,
        common::test_policy(),
    );
    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site register my-blog",
        ))
        .await
        .expect("process")
    );
    assert!(
        store
            .get_site_auth("my-blog")
            .await
            .expect("site")
            .is_some()
    );
    let applied = store.list_applied_claims().await.expect("applied claims");
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].user_id, "@alice:hs");
    assert_eq!(applied[0].level, SITE_ADMIN_LEVEL);
    assert_eq!(
        driver
            .power_levels
            .lock()
            .await
            .get("!space-my-blog:hs")
            .unwrap()["users"]["@alice:hs"],
        SITE_ADMIN_LEVEL
    );
    let replies = driver.replies.lock().await;
    assert!(
        replies
            .iter()
            .any(|(_, body)| body.contains("已登记为本站第一个站点管理员"))
    );
}

#[tokio::test]
async fn admin_can_retire_a_page_room_with_confirm() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("admin-page-retire"))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("attach space");
    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@alice:hs")).with_power_levels(
            "!space:hs",
            power_levels(serde_json::json!({ "@alice:hs": 100 })),
        ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver,
        Vec::new(),
        None,
        common::test_policy(),
    );

    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog page hello retire",
        ))
        .await
        .expect("process")
    );
    assert_eq!(
        store
            .get_room_status("!room:hs")
            .await
            .expect("room status"),
        Some(RoomStatus::Active),
        "confirmation must not retire yet"
    );
    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog page hello retire --confirm",
        ))
        .await
        .expect("process")
    );
    assert_eq!(
        store
            .get_room_status("!room:hs")
            .await
            .expect("room status"),
        Some(RoomStatus::Retired)
    );
}

#[tokio::test]
async fn operator_can_retire_a_room_by_id() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("operator-room-retire"))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("site id");
    let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    store
        .register_room("!room:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    let p = processor(
        store.clone(),
        private_members("@op:hs"),
        vec!["@op:hs".to_string()],
    );

    assert!(
        p.process_bot_command(&command_message("@op:hs", "!cumments room !room:hs retire"))
            .await
            .expect("process")
    );
    assert_eq!(
        store
            .get_room_status("!room:hs")
            .await
            .expect("room status"),
        Some(RoomStatus::Active)
    );
    assert!(
        p.process_bot_command(&command_message(
            "@op:hs",
            "!cumments room !room:hs retire --confirm",
        ))
        .await
        .expect("process")
    );
    assert_eq!(
        store
            .get_room_status("!room:hs")
            .await
            .expect("room status"),
        Some(RoomStatus::Retired)
    );
}

#[tokio::test]
async fn commands_outside_private_channel_are_consumed_silently() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("not-private"))
            .await
            .expect("connect db"),
    );
    let p = processor(
        store.clone(),
        vec![
            "@_cumments_bot:hs".to_string(),
            "@alice:hs".to_string(),
            "@mallory:hs".to_string(),
        ],
        Vec::new(),
    );
    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site register my-blog",
        ))
        .await
        .expect("process")
    );
    assert!(
        store
            .get_site_auth("my-blog")
            .await
            .expect("site")
            .is_none()
    );
}

#[tokio::test]
async fn backfill_queues_for_operator_and_rejects_busy() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("backfill"))
            .await
            .expect("connect db"),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let p = processor_with(
        store.clone(),
        private_members(":hs"),
        vec![":hs".to_string()],
        Some(tx.clone()),
        common::test_policy(),
    );
    assert!(
        p.process_bot_command(&command_message(":hs", "!cumments backfill 10",))
            .await
            .expect("process")
    );
    // Channel is full while the worker is busy: the second command reports
    // busy instead of queueing.
    assert!(
        p.process_bot_command(&command_message(":hs", "!cumments backfill"))
            .await
            .expect("process")
    );
    let request = rx.try_recv().expect("exactly one backfill queued");
    assert_eq!(request.actor_mxid, ":hs");
    assert_eq!(request.max_pages, 10);
    assert!(
        rx.try_recv().is_err(),
        "busy backfill must not queue a second request"
    );

    // Non-operators are denied before touching the queue.
    let denied = processor_with(
        store.clone(),
        private_members("@mallory:hs"),
        Vec::new(),
        Some(tx),
        common::test_policy(),
    );
    assert!(
        denied
            .process_bot_command(&command_message("@mallory:hs", "!cumments backfill",))
            .await
            .expect("process")
    );
}

#[tokio::test]
async fn operator_sites_list_includes_config_only_sites() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("config-sites"))
            .await
            .expect("connect db"),
    );
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    let policy = Arc::new(SiteAuthPolicy {
        verification: SiteVerificationPolicy::Optional,
        sites: [(
            "config-blog".to_string(),
            SitePolicyEntry {
                auth_mode: Some(SiteAuthMode::Origin),
                allowed_origins: Vec::new(),
                secret: None,
            },
        )]
        .into_iter()
        .collect(),
    });
    let driver = Arc::new(common::TestDriver::with_joined_members(private_members(
        ":hs",
    )));
    let p = processor_with_driver(store, driver.clone(), vec![":hs".to_string()], None, policy);
    assert!(
        p.process_bot_command(&command_message(":hs", "!cumments sites list"))
            .await
            .expect("process")
    );
    let replies = driver.replies.lock().await;
    let reply = replies
        .iter()
        .find(|(_, body)| body.contains("my-blog"))
        .expect("list reply");
    assert!(reply.1.contains("config-blog（配置）"));
}

fn power_levels(users: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "users": users,
        "events": { "m.room.power_levels": 100 },
        "state_default": 50,
    })
}

async fn sticker_site(store: &DbStore) {
    store
        .register_site("my-blog", &token_hash("claim"), true)
        .await
        .expect("register site");
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("attach space");
}

#[tokio::test]
async fn manager_can_manage_stickers_but_not_governance() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("manager-stickers"))
            .await
            .expect("connect db"),
    );
    sticker_site(&store).await;
    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@bob:hs")).with_power_levels(
            "!space:hs",
            power_levels(serde_json::json!({
                "@alice:hs": 100,
                "@bob:hs": 75,
            })),
        ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver.clone(),
        Vec::new(),
        None,
        common::test_policy(),
    );

    // Managers may manage stickers (state_default 50 < 75)...
    assert!(
        p.process_bot_command(&command_message(
            "@bob:hs",
            "!cumments site my-blog sticker add default cat mxc://hs/1",
        ))
        .await
        .expect("process")
    );
    let writes = driver.state_writes.lock().await;
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].2, "default");
    drop(writes);

    // ...but not governance (the power-levels event is locked at 100).
    assert!(
        p.process_bot_command(&command_message(
            "@bob:hs",
            "!cumments site my-blog manager add @carol:hs",
        ))
        .await
        .expect("process")
    );
    let audit = store
        .list_command_audit(Some("@bob:hs"), 10)
        .await
        .expect("audit");
    assert_eq!(audit[0].status, CommandAuditStatus::Denied);

    // Post retirement is the same governance fence: denied for managers.
    assert!(
        p.process_bot_command(&command_message(
            "@bob:hs",
            "!cumments site my-blog page hello retire --confirm",
        ))
        .await
        .expect("process")
    );
    let audit = store
        .list_command_audit(Some("@bob:hs"), 10)
        .await
        .expect("audit");
    assert_eq!(audit[0].status, CommandAuditStatus::Denied);
}

#[tokio::test]
async fn sticker_remove_requires_confirm_and_executes() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("sticker-remove"))
            .await
            .expect("connect db"),
    );
    sticker_site(&store).await;
    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@alice:hs")).with_power_levels(
            "!space:hs",
            power_levels(serde_json::json!({ "@alice:hs": 100 })),
        ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver.clone(),
        Vec::new(),
        None,
        common::test_policy(),
    );

    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog sticker add default cat mxc://hs/1",
        ))
        .await
        .expect("add")
    );
    assert_eq!(driver.state_writes.lock().await.len(), 1);

    // Without --confirm nothing is written.
    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog sticker remove default cat",
        ))
        .await
        .expect("remove prompt")
    );
    assert_eq!(
        driver.state_writes.lock().await.len(),
        1,
        "remove must wait for confirmation"
    );

    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog sticker remove default cat --confirm",
        ))
        .await
        .expect("remove confirmed")
    );
    assert_eq!(driver.state_writes.lock().await.len(), 2);
    let state = driver
        .room_state
        .lock()
        .await
        .get(&(
            "!space:hs".to_string(),
            "m.room.image_pack".to_string(),
            "default".to_string(),
        ))
        .cloned()
        .expect("pack state");
    assert!(
        state["images"]
            .as_object()
            .is_some_and(|images| images.is_empty())
    );
}

#[tokio::test]
async fn stranger_cannot_manage_stickers() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("stranger-stickers"))
            .await
            .expect("connect db"),
    );
    sticker_site(&store).await;
    let driver = Arc::new(
        common::TestDriver::with_joined_members(private_members("@eve:hs")).with_power_levels(
            "!space:hs",
            power_levels(serde_json::json!({ "@alice:hs": 100 })),
        ),
    );
    let p = processor_with_driver(
        store.clone(),
        driver,
        Vec::new(),
        None,
        common::test_policy(),
    );

    assert!(
        p.process_bot_command(&command_message(
            "@eve:hs",
            "!cumments site my-blog sticker add default cat mxc://hs/1",
        ))
        .await
        .expect("process")
    );
    let audit = store
        .list_command_audit(Some("@eve:hs"), 10)
        .await
        .expect("audit");
    assert_eq!(audit[0].status, CommandAuditStatus::Denied);
}

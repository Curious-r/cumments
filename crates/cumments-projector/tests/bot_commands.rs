use cumments_core::{
    audit::CommandAuditStatus,
    governance::{OWNER_LEVEL, RoleEntry},
    models::{Content, TextContent, TextStyle},
    ports::{CommandAuditStore, GovernanceStore, RoleClaimStore, SiteAuthStore},
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

fn processor(
    store: Arc<DbStore>,
    members: Vec<String>,
    admin_mxids: Vec<String>,
) -> EventProcessor {
    processor_with(store, members, admin_mxids, None, common::test_policy())
}

fn processor_with(
    store: Arc<DbStore>,
    members: Vec<String>,
    admin_mxids: Vec<String>,
    backfill_tx: Option<tokio::sync::mpsc::Sender<cumments_projector::backfill::BackfillRequest>>,
    policy: std::sync::Arc<cumments_core::site_auth::SiteAuthPolicy>,
) -> EventProcessor {
    processor_with_driver(
        store,
        Arc::new(common::TestDriver::with_joined_members(members)),
        admin_mxids,
        backfill_tx,
        policy,
    )
}

fn processor_with_driver(
    store: Arc<DbStore>,
    driver: Arc<common::TestDriver>,
    admin_mxids: Vec<String>,
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
        role_claim_store: store.clone(),
        submission_store: store.clone(),
        audit_store: store.clone(),
        site_auth_store: store.clone(),
        site_auth_policy: policy,
        site_service: Arc::new(cumments_core::site_service::SiteService::new(
            store.clone() as Arc<dyn cumments_core::ports::SiteStore>
        )),
        driver: Some(driver),
        admin_mxids,
        backfill_tx,
        event_bus: tx,
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
async fn admin_sites_list_works_and_unknown_user_is_denied() {
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
        private_members("@admin:hs"),
        vec!["@admin:hs".to_string()],
    );
    assert!(
        p.process_bot_command(&command_message("@admin:hs", "!cumments sites list"))
            .await
            .expect("process"),
        "admin command consumed"
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
async fn owner_can_create_co_manager_claim_and_retire_with_confirm() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("owner"))
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
                level: OWNER_LEVEL,
            }],
        )
        .await
        .expect("project owner");

    let p = processor(store.clone(), private_members("@alice:hs"), Vec::new());
    assert!(
        p.process_bot_command(&command_message(
            "@alice:hs",
            "!cumments site my-blog co-manager add @bob:hs",
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
async fn site_registration_is_public_self_service() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("register"))
            .await
            .expect("connect db"),
    );
    let p = processor(store.clone(), private_members("@alice:hs"), Vec::new());
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
async fn backfill_queues_for_admin_and_rejects_busy() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("backfill"))
            .await
            .expect("connect db"),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let p = processor_with(
        store.clone(),
        private_members("@admin:hs"),
        vec!["@admin:hs".to_string()],
        Some(tx.clone()),
        common::test_policy(),
    );
    assert!(
        p.process_bot_command(&command_message("@admin:hs", "!cumments backfill 10",))
            .await
            .expect("process")
    );
    // Channel is full while the worker is busy: the second command reports
    // busy instead of queueing.
    assert!(
        p.process_bot_command(&command_message("@admin:hs", "!cumments backfill"))
            .await
            .expect("process")
    );
    let request = rx.try_recv().expect("exactly one backfill queued");
    assert_eq!(request.actor_mxid, "@admin:hs");
    assert_eq!(request.max_pages, 10);
    assert!(
        rx.try_recv().is_err(),
        "busy backfill must not queue a second request"
    );

    // Non-admins are denied before touching the queue.
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
async fn admin_sites_list_includes_config_only_sites() {
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
        "@admin:hs",
    )));
    let p = processor_with_driver(
        store,
        driver.clone(),
        vec!["@admin:hs".to_string()],
        None,
        policy,
    );
    assert!(
        p.process_bot_command(&command_message("@admin:hs", "!cumments sites list"))
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

use cumments_core::audit::{CommandAuditStatus, NewCommandAuditEntry};
use cumments_core::ports::CommandAuditStore;
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-audit-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn command_audit_records_and_lists_with_filter() {
    let store = DbStore::connect(&test_db_url("audit"))
        .await
        .expect("connect db");
    for (actor, status) in [
        ("@alice:hs", CommandAuditStatus::Ok),
        ("@alice:hs", CommandAuditStatus::Denied),
        ("@bob:hs", CommandAuditStatus::Error),
    ] {
        store
            .record_command_audit(&NewCommandAuditEntry {
                actor_mxid: actor.to_string(),
                room_id: "!dm:hs".to_string(),
                command: "!cumments site my-blog status".to_string(),
                site_id: Some("my-blog".to_string()),
                status,
                error: None,
            })
            .await
            .expect("record audit");
    }

    let all = store.list_command_audit(None, 10).await.expect("list all");
    assert_eq!(all.len(), 3);
    assert!(all[0].created_at >= all[1].created_at);

    let alice = store
        .list_command_audit(Some("@alice:hs"), 10)
        .await
        .expect("list alice");
    assert_eq!(alice.len(), 2);
    assert!(alice.iter().all(|e| e.actor_mxid == "@alice:hs"));

    assert_eq!(store.count_command_audit(None).await.expect("count"), 3);
    assert_eq!(
        store
            .count_command_audit(Some("@alice:hs"))
            .await
            .expect("filtered count"),
        2
    );
}

use cumments_store::migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::prelude::*;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

fn migration_names() -> Vec<String> {
    Migrator::migrations()
        .into_iter()
        .map(|m| m.name().to_string())
        .collect()
}

async fn column_names(db: &sea_orm::DatabaseConnection, table: &str) -> Vec<String> {
    let sql = format!("PRAGMA table_info({table})");
    let rows = db
        .query_all_raw(Statement::from_string(DbBackend::Sqlite, sql))
        .await
        .expect("query table info");
    rows.iter()
        .filter_map(|row| row.try_get::<String>("", "name").ok())
        .collect()
}

async fn column_not_null(db: &sea_orm::DatabaseConnection, table: &str, column: &str) -> bool {
    let sql = format!("PRAGMA table_info({table})");
    let rows = db
        .query_all_raw(Statement::from_string(DbBackend::Sqlite, sql))
        .await
        .expect("query table info");
    rows.iter()
        .find_map(|row| {
            let name = row.try_get::<String>("", "name").ok()?;
            (name == column).then(|| row.try_get::<i64>("", "notnull").unwrap_or(1) != 0)
        })
        .unwrap_or(false)
}

#[tokio::test]
async fn submission_txn_migrations_are_registered() {
    let names = migration_names();
    assert!(
        names.contains(&"m20260815_000045_post_submission_txn_id".to_string()),
        "000045 must be registered or upgrades from 0.23.2 miss post_submissions.txn_id"
    );
    assert!(
        names.contains(&"m20260815_000046_unified_submission_txn_ids".to_string()),
        "000046 must be registered or upgrades from 0.23.2 miss delete/update txn columns"
    );
    assert!(
        names.contains(&"m20260815_000047_role_claim_dm_room".to_string()),
        "000047 must be registered or claim DMs cannot be tracked"
    );
    assert!(
        names.contains(&"m20260815_000048_media_upload_submission".to_string()),
        "000048 must be registered or orphan cleanup can delete retrying media"
    );
    assert!(
        names.contains(&"m20260815_000049_command_audit_log".to_string()),
        "000049 must be registered or chat command audit records are lost"
    );
    assert!(
        names.contains(&"m20260816_000051_media_uploads_post_slug_nullable".to_string()),
        "000051 must be registered or site-scoped avatar uploads cannot share the upload table"
    );
    assert!(
        names.contains(&"m20260823_000054_redacted_content".to_string()),
        "000054 must be registered or existing redacted comments retain deleted content"
    );
}

#[tokio::test]
async fn redacted_content_migration_sanitizes_existing_rows() {
    let url = test_db_url("redacted-content");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, Some(53))
        .await
        .expect("migrate to 000053");

    let now = chrono::Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO messages \
         (event_id, room_id, site_id, page_slug, sender_mxid, author_kind, content_json, \
          raw_content_json, timestamp, status, last_edit_ts, reply_to, thread_root, \
          submission_id, created_at, updated_at) \
         VALUES \
         ('$redacted:hs', '!room:hs', 'my-blog', 'hello', '@alice:hs', 'visitor', \
          '{{\"body\":\"secret\"}}', '{{\"body\":\"secret\"}}', '{now}', 'redacted', \
          123, '$parent:hs', '$thread:hs', 42, '{now}', '{now}')"
    ))
    .await
    .expect("insert redacted message");
    db.execute_unprepared(&format!(
        "INSERT INTO message_revisions \
         (event_id, message_event_id, content_json, edited_at, editor_mxid, created_at) \
         VALUES \
         ('$edit:hs', '$redacted:hs', '{{\"body\":\"edited secret\"}}', '{now}', \
          '@alice:hs', '{now}')"
    ))
    .await
    .expect("insert redacted revision");

    Migrator::up(&db, None).await.expect("apply 000054");

    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT content_json, raw_content_json, last_edit_ts, reply_to, thread_root, \
             submission_id FROM messages WHERE event_id = '$redacted:hs'",
        ))
        .await
        .expect("query migrated redaction");
    assert_eq!(rows.len(), 1);
    let content: String = rows[0].try_get("", "content_json").expect("content");
    let raw: String = rows[0]
        .try_get("", "raw_content_json")
        .expect("raw content");
    let last_edit_ts: Option<i64> = rows[0].try_get("", "last_edit_ts").expect("last edit");
    let reply_to: Option<String> = rows[0].try_get("", "reply_to").expect("reply");
    let thread_root: Option<String> = rows[0].try_get("", "thread_root").expect("thread root");
    let submission_id: Option<i64> = rows[0].try_get("", "submission_id").expect("submission");
    assert_eq!(content, r#"{"type":"redacted"}"#);
    assert_eq!(raw, "{}");
    assert_eq!(last_edit_ts, None);
    assert_eq!(reply_to, None);
    assert_eq!(thread_root, None);
    assert_eq!(submission_id, None);

    let revisions = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT event_id FROM message_revisions \
             WHERE message_event_id = '$redacted:hs'",
        ))
        .await
        .expect("query migrated revisions");
    assert!(
        revisions.is_empty(),
        "edit history for a redacted comment must be removed"
    );
}

#[tokio::test]
async fn upgrading_from_0044_schema_adds_txn_columns() {
    let url = test_db_url("upgrade-0044");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, None).await.expect("fresh migrate");

    // Rewind to the post-000044 (v0.23.2) schema: undo what 000045/000046
    // own, so re-running the migrator exercises the real upgrade path.
    db.execute_unprepared(
        "DELETE FROM seaql_migrations WHERE version IN \
         ('m20260815_000045_post_submission_txn_id', \
          'm20260815_000046_unified_submission_txn_ids', \
          'm20260815_000047_role_claim_dm_room', \
          'm20260815_000048_media_upload_submission', \
          'm20260815_000049_command_audit_log')",
    )
    .await
    .expect("un-apply txn migrations");
    for sql in [
        "ALTER TABLE post_submissions DROP COLUMN txn_id",
        "ALTER TABLE delete_submissions DROP COLUMN txn_id",
        "ALTER TABLE delete_submissions DROP COLUMN matrix_event_id",
        "ALTER TABLE update_submissions DROP COLUMN txn_id",
        "ALTER TABLE update_submissions DROP COLUMN matrix_event_id",
        "ALTER TABLE post_submissions \
         ADD COLUMN force_new_txn BOOLEAN NOT NULL DEFAULT 0",
        "ALTER TABLE role_claims DROP COLUMN dm_room_id",
        "ALTER TABLE media_uploads DROP COLUMN submission_id",
        "DROP TABLE command_audit_logs",
    ] {
        db.execute_unprepared(sql).await.expect("rewind schema");
    }

    Migrator::up(&db, None).await.expect("upgrade migrate");

    let post_columns = column_names(&db, "post_submissions").await;
    let delete_columns = column_names(&db, "delete_submissions").await;
    let update_columns = column_names(&db, "update_submissions").await;
    let claim_columns = column_names(&db, "role_claims").await;
    let media_columns = column_names(&db, "media_uploads").await;

    assert!(post_columns.iter().any(|c| c == "txn_id"));
    assert!(!post_columns.iter().any(|c| c == "force_new_txn"));
    for columns in [&delete_columns, &update_columns] {
        assert!(columns.iter().any(|c| c == "txn_id"));
        assert!(columns.iter().any(|c| c == "matrix_event_id"));
    }
    assert!(claim_columns.iter().any(|c| c == "dm_room_id"));
    assert!(media_columns.iter().any(|c| c == "submission_id"));
    assert!(
        !column_not_null(&db, "media_uploads", "page_slug").await,
        "page_slug must be nullable so avatar uploads are site-scoped"
    );
    let audit_columns = column_names(&db, "command_audit_logs").await;
    assert!(audit_columns.iter().any(|c| c == "actor_mxid"));
    assert!(audit_columns.iter().any(|c| c == "created_at"));
}

#[tokio::test]
async fn terminology_rename_migration_converges_legacy_schema() {
    let url = test_db_url("terminology-rename");
    let db = Database::connect(&url).await.expect("connect db");

    // Entity-first migrations already create `page_slug` on fresh databases,
    // so reshape the tables back to the pre-rename shape to simulate a
    // database created before 000052.
    Migrator::up(&db, Some(51))
        .await
        .expect("migrate to 000051");
    for table in [
        "messages",
        "room_registry",
        "media_uploads",
        "update_submissions",
    ] {
        if column_names(&db, table)
            .await
            .iter()
            .any(|c| c == "page_slug")
        {
            db.execute_unprepared(&format!(
                "ALTER TABLE {table} RENAME COLUMN page_slug TO post_slug"
            ))
            .await
            .expect("simulate pre-rename schema");
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO messages \
         (event_id, room_id, site_id, post_slug, sender_mxid, author_kind, content_json, \
          raw_content_json, timestamp, status, created_at, updated_at) \
         VALUES \
         ('$visitor:hs', '!room:hs', 'my-blog', 'hello', '@_cumments_my-blog_x:hs', 'guest', \
          '{{}}', '{{}}', '{now}', 'active', '{now}', '{now}')"
    ))
    .await
    .expect("insert legacy message");

    Migrator::up(&db, None).await.expect("apply 000052");

    for table in [
        "messages",
        "room_registry",
        "media_uploads",
        "update_submissions",
    ] {
        let columns = column_names(&db, table).await;
        assert!(
            columns.iter().any(|c| c == "page_slug"),
            "{table} must use page_slug after 000052: {columns:?}"
        );
        assert!(
            !columns.iter().any(|c| c == "post_slug"),
            "{table} must not retain post_slug after 000052: {columns:?}"
        );
    }

    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT author_kind FROM messages WHERE event_id = '$visitor:hs'",
        ))
        .await
        .expect("query migrated message");
    assert_eq!(rows.len(), 1);
    let kind: String = rows[0].try_get("", "author_kind").expect("author_kind");
    assert_eq!(kind, "visitor", "guest author kind must be rewritten");
}

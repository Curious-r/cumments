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
    assert!(
        names.contains(&"m20260824_000055_edit_revision_facts".to_string()),
        "000055 must be registered or edit revisions cannot be redacted independently"
    );
    assert!(
        names.contains(&"m20260824_000056_poll_response_events".to_string()),
        "000056 must be registered or poll votes remain lossy per-voter rows"
    );
    assert!(
        names.contains(&"m20260825_000057_appservice_txn_dedupe".to_string()),
        "000057 must be registered or AppService transaction dedupe is not durable"
    );
    assert!(
        names.contains(&"m20260825_000059_room_upgrade_intents".to_string()),
        "000059 must be registered or native room upgrades have no durable authorization"
    );
    assert!(
        names.contains(&"m20260825_000060_sanitize_redacted_payloads".to_string()),
        "000060 must be registered or redacted payloads can remain in SQLite"
    );
    assert!(
        names.contains(&"m20260825_000061_clear_redacted_poll_choices".to_string()),
        "000061 must be registered or redacted poll choices can remain in SQLite"
    );
    assert!(
        names.contains(&"m20260826_000063_poll_answer_selections".to_string()),
        "000063 must be registered or MSC3381 selections are lossy"
    );
    assert!(
        names.contains(&"m20260827_000067_drop_poll_end_authorized".to_string()),
        "000067 must be registered or the removed end-authorization snapshot survives upgrades"
    );
    assert!(
        names.contains(&"m20260827_000068_operation_claims".to_string()),
        "000068 must be registered or operation identity is not server-wide unique"
    );
    assert!(
        names.contains(&"m20260915_000071_media_references".to_string()),
        "000071 must be registered or media_references table is missing"
    );
    assert!(
        names.contains(&"m20260915_000072_profile_operations".to_string()),
        "000072 must be registered or profile_operations table is missing"
    );
    assert!(
        names.contains(&"m20260915_000073_profile_operation_site_scope".to_string()),
        "000073 must be registered or profile_operations site scope index is missing"
    );
    assert!(
        names.contains(&"m20260915_000074_profile_operation_unique_sequence".to_string()),
        "000074 must be registered or profile_operations unique sequence index is missing"
    );
    assert!(
        names.contains(&"m20260915_000075_room_members_media_reference".to_string()),
        "000075 must be registered or room_members media_reference column is missing"
    );
    assert!(
        names.contains(&"m20260915_000076_room_members_projection_ordering".to_string()),
        "000076 must be registered or room_members projection ordering columns are missing"
    );
    assert!(
        names.contains(&"m20260915_000077_messages_author_media_reference".to_string()),
        "000077 must be registered or messages author_media_reference column is missing"
    );
}

#[tokio::test]
async fn operation_claims_table_enforces_server_wide_uniqueness() {
    let url = test_db_url("operation-claims");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, None).await.expect("migrate to latest");

    // An operation claim carries only operation identity — no submission.
    assert!(
        !column_names(&db, "operation_claims")
            .await
            .iter()
            .any(|column| column == "submission_id"),
        "operation claims must not be coupled to durable submissions"
    );

    let now = chrono::Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO operation_claims \
         (operation_id, author_public_key, fingerprint, created_at) \
         VALUES ('op-1', 'author-a', 'fp-a', '{now}')"
    ))
    .await
    .expect("first claim");

    // The unique index on operation_id must reject a second, different claim
    // for the same server-wide operation id.
    let duplicate = db
        .execute_unprepared(&format!(
            "INSERT INTO operation_claims \
             (operation_id, author_public_key, fingerprint, created_at) \
             VALUES ('op-1', 'author-b', 'fp-b', '{now}')"
        ))
        .await;
    assert!(
        duplicate.is_err(),
        "a second claim for the same operation_id must be rejected by the database"
    );

    // Different operation ids remain independent.
    db.execute_unprepared(&format!(
        "INSERT INTO operation_claims \
         (operation_id, author_public_key, fingerprint, created_at) \
         VALUES ('op-2', 'author-b', 'fp-b', '{now}')"
    ))
    .await
    .expect("independent operation id");
}

#[tokio::test]
async fn operation_claim_decoupling_preserves_existing_records() {
    let url = test_db_url("operation-claim-decoupling");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, Some(68))
        .await
        .expect("migrate to 000068");

    // Simulate a database that ran the earlier schema, where the claim held the
    // durable submission id and the submission had no operation reference.
    db.execute_unprepared("ALTER TABLE operation_claims ADD COLUMN submission_id INTEGER")
        .await
        .expect("simulate earlier claim schema");
    let now = chrono::Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO post_submissions \
         (id, payload, status, retry_count, timeout_confirmations, timeout_check_errors, \
          created_at, updated_at) \
         VALUES (7, '{{\"poll\":true}}', 'pending', 0, 0, 0, '{now}', '{now}')"
    ))
    .await
    .expect("legacy submission");
    db.execute_unprepared(&format!(
        "INSERT INTO operation_claims \
         (operation_id, author_public_key, fingerprint, submission_id, created_at) \
         VALUES ('op-legacy', 'author-a', 'fp-a', 7, '{now}')"
    ))
    .await
    .expect("legacy claim");

    Migrator::up(&db, None).await.expect("apply 000069");

    // The claim survives, the old coupling is gone, and the submission now
    // carries the operation reference.
    assert!(
        !column_names(&db, "operation_claims")
            .await
            .iter()
            .any(|column| column == "submission_id"),
        "the coupling column must be dropped"
    );
    let claims = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT operation_id, author_public_key, fingerprint FROM operation_claims",
        ))
        .await
        .expect("claims");
    assert_eq!(claims.len(), 1);
    assert_eq!(
        claims[0].try_get::<String>("", "operation_id").unwrap(),
        "op-legacy"
    );
    assert_eq!(
        claims[0]
            .try_get::<String>("", "author_public_key")
            .unwrap(),
        "author-a"
    );

    let submissions = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT operation_id FROM post_submissions WHERE id = 7",
        ))
        .await
        .expect("submissions");
    assert_eq!(
        submissions[0]
            .try_get::<Option<String>>("", "operation_id")
            .unwrap()
            .as_deref(),
        Some("op-legacy"),
        "the legacy submission id must be preserved as an operation reference"
    );
}

#[tokio::test]
async fn drop_poll_end_authorized_migration_removes_projection_snapshot() {
    let url = test_db_url("drop-poll-end-authorized");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, Some(66))
        .await
        .expect("migrate to 000066");

    // Simulate a database that ran the earlier schema, where a projection-time
    // authorization snapshot was persisted as a NOT NULL column.
    db.execute_unprepared(
        "ALTER TABLE poll_end_events ADD COLUMN authorized BOOLEAN NOT NULL DEFAULT 0",
    )
    .await
    .expect("simulate earlier schema");
    assert!(
        column_names(&db, "poll_end_events")
            .await
            .iter()
            .any(|column| column == "authorized")
    );

    Migrator::up(&db, None).await.expect("apply 000067");

    assert!(
        !column_names(&db, "poll_end_events")
            .await
            .iter()
            .any(|column| column == "authorized"),
        "the projection-time authorization snapshot must not survive"
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
          raw_content_json, matrix_event_type, timestamp, status, last_edit_ts, reply_to, thread_root, \
          submission_id, original_content_json, created_at, updated_at) \
         VALUES \
         ('$redacted:hs', '!room:hs', 'my-blog', 'hello', '@alice:hs', 'visitor', \
          '{{\"body\":\"secret\"}}', '{{\"body\":\"secret\"}}', 'm.room.message', '{now}', \
          'redacted', 123, '$parent:hs', '$thread:hs', 42, \
          '{{\"type\":\"redacted\"}}', '{now}', '{now}')"
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
async fn sanitize_redacted_payloads_migration_clears_late_retained_bodies() {
    let url = test_db_url("sanitize-redacted-payloads");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, Some(59))
        .await
        .expect("migrate to 000059");

    let now = chrono::Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO messages \
         (event_id, room_id, site_id, page_slug, sender_mxid, author_kind, content_json, \
          original_content_json, matrix_event_type, raw_content_json, timestamp, status, \
          created_at, updated_at) \
         VALUES \
         ('$redacted:hs', '!room:hs', 'my-blog', 'hello', '@alice:hs', 'visitor', \
          '{{\"type\":\"redacted\"}}', '{{\"body\":\"secret\"}}', 'm.room.message', '{{}}', \
          '{now}', 'redacted', '{now}', '{now}')"
    ))
    .await
    .expect("insert message deleted after 000060's baseline");
    db.execute_unprepared(&format!(
        "INSERT INTO message_revisions \
         (event_id, message_event_id, content_json, edited_at, editor_mxid, redacted_at, \
          redacted_by, created_at) \
         VALUES \
         ('$edit:hs', '$event:hs', '{{\"body\":\"edited secret\"}}', '{now}', '@alice:hs', \
          '{now}', '@moderator:hs', '{now}')"
    ))
    .await
    .expect("insert individually redacted revision");

    Migrator::up(&db, None).await.expect("apply 000060");

    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT original_content_json FROM messages WHERE event_id = '$redacted:hs'",
        ))
        .await
        .expect("query migrated message");
    assert_eq!(rows.len(), 1);
    let original: String = rows[0]
        .try_get("", "original_content_json")
        .expect("original content");
    assert_eq!(original, r#"{"type":"redacted"}"#);

    let revisions = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT content_json FROM message_revisions WHERE event_id = '$edit:hs'",
        ))
        .await
        .expect("query migrated revision");
    assert_eq!(revisions.len(), 1);
    let content: String = revisions[0].try_get("", "content_json").expect("content");
    assert_eq!(content, r#"{"type":"redacted"}"#);
}

#[tokio::test]
async fn clear_redacted_poll_choices_migration_forgets_selected_options() {
    let url = test_db_url("clear-redacted-poll-choices");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, Some(60))
        .await
        .expect("migrate to 000060");

    let now = chrono::Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO poll_response_events \
         (event_id, poll_message_id, sender_mxid, option_index, origin_server_ts, \
          answer_ids_json, spoiled_reason, redacted_at, redacted_by, created_at) \
         VALUES \
         ('$vote:hs', '$poll:hs', '@alice:hs', 2, 1, '[]', NULL, '{now}', \
          '@moderator:hs', '{now}')"
    ))
    .await
    .expect("insert redacted vote");

    Migrator::up(&db, None).await.expect("apply 000061");

    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT option_index FROM poll_response_events WHERE event_id = '$vote:hs'",
        ))
        .await
        .expect("query migrated vote");
    assert_eq!(rows.len(), 1);
    let option_index: Option<i64> = rows[0].try_get("", "option_index").expect("option");
    assert_eq!(option_index, None);
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
          raw_content_json, matrix_event_type, timestamp, status, original_content_json, created_at, updated_at) \
         VALUES \
         ('$visitor:hs', '!room:hs', 'my-blog', 'hello', '@_cumments_my-blog_x:hs', 'guest', \
          '{{}}', 'm.room.message', '{now}', '{now}', 'active', '{{}}', '{now}', '{now}')"
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

#[tokio::test]
async fn media_references_table_permits_same_mxc_across_sites_and_rejects_duplicates_within_site() {
    let url = test_db_url("media-references");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, None).await.expect("migrate to latest");

    let now = chrono::Utc::now().to_rfc3339();
    // 1. Insert on site-a
    db.execute_unprepared(&format!(
        "INSERT INTO media_references \
         (media_reference, site_id, mxc_uri, is_external, created_at) \
         VALUES ('cumments-media:550e8400-e29b-41d4-a716-446655440000', 'site-a', 'mxc://hs/1', 0, '{now}')"
    ))
    .await
    .expect("first media reference");

    // 2. Same MXC URI on site-b with a different media_reference MUST SUCCEED (not globally unique)
    db.execute_unprepared(&format!(
        "INSERT INTO media_references \
         (media_reference, site_id, mxc_uri, is_external, created_at) \
         VALUES ('cumments-media:550e8400-e29b-41d4-a716-446655440001', 'site-b', 'mxc://hs/1', 0, '{now}')"
    ))
    .await
    .expect("same mxc on different site is allowed");

    // 3. Duplicate (site_id, mxc_uri) on site-a MUST FAIL
    let dup_site_mxc = db
        .execute_unprepared(&format!(
            "INSERT INTO media_references \
             (media_reference, site_id, mxc_uri, is_external, created_at) \
             VALUES ('cumments-media:550e8400-e29b-41d4-a716-446655440002', 'site-a', 'mxc://hs/1', 0, '{now}')"
        ))
        .await;
    assert!(
        dup_site_mxc.is_err(),
        "duplicate (site_id, mxc_uri) must be rejected by unique constraint"
    );

    // 4. Duplicate media_reference (primary key) MUST FAIL
    let dup_pk = db
        .execute_unprepared(&format!(
            "INSERT INTO media_references \
             (media_reference, site_id, mxc_uri, is_external, created_at) \
             VALUES ('cumments-media:550e8400-e29b-41d4-a716-446655440000', 'site-c', 'mxc://hs/2', 0, '{now}')"
        ))
        .await;
    assert!(dup_pk.is_err(), "duplicate primary key must be rejected");
}

#[tokio::test]
async fn profile_operations_table_enforces_unique_operation_id_and_tracks_fields() {
    let url = test_db_url("profile-operations");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, None).await.expect("migrate to latest");

    let now = chrono::Utc::now().to_rfc3339();
    // 1. Insert first profile operation
    db.execute_unprepared(&format!(
        "INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-prof-1', 'author-1', 'site-a', 'display_name', '{{\"kind\":\"set_display_name\",\"value\":\"Alice\"}}', 'pending', 1, '{now}', '{now}')"
    ))
    .await
    .expect("insert profile operation");

    // 2. Duplicate operation_id MUST FAIL
    let dup_op = db
        .execute_unprepared(&format!(
            "INSERT INTO profile_operations \
             (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
             VALUES ('op-prof-1', 'author-2', 'site-b', 'avatar', NULL, 'pending', 1, '{now}', '{now}')"
        ))
        .await;
    assert!(dup_op.is_err(), "duplicate operation_id must be rejected");

    // 3. Different operation_id for same author/field with incremented sequence succeeds
    db.execute_unprepared(&format!(
        "INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-prof-2', 'author-1', 'site-a', 'display_name', '{{\"kind\":\"clear_display_name\"}}', 'completed', 2, '{now}', '{now}')"
    ))
    .await
    .expect("second profile operation with incremented sequence");

    // 4. Duplicate (site_id, author_public_key, field, sequence) MUST FAIL
    let dup_seq = db
        .execute_unprepared(&format!(
            "INSERT INTO profile_operations \
             (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
             VALUES ('op-prof-3', 'author-1', 'site-a', 'display_name', NULL, 'pending', 1, '{now}', '{now}')"
        ))
        .await;
    assert!(
        dup_seq.is_err(),
        "duplicate (site_id, author_public_key, field, sequence) must be rejected"
    );

    // 5. Same sequence on different site is allowed
    db.execute_unprepared(&format!(
        "INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-prof-4', 'author-1', 'site-b', 'display_name', NULL, 'pending', 1, '{now}', '{now}')"
    ))
    .await
    .expect("same sequence on different site is allowed");

    // 6. Same sequence on different field is allowed
    db.execute_unprepared(&format!(
        "INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-prof-5', 'author-1', 'site-a', 'avatar', NULL, 'pending', 1, '{now}', '{now}')"
    ))
    .await
    .expect("same sequence on different field is allowed");

    // 7. Same sequence for different author is allowed
    db.execute_unprepared(&format!(
        "INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-prof-6', 'author-2', 'site-a', 'display_name', NULL, 'pending', 1, '{now}', '{now}')"
    ))
    .await
    .expect("same sequence for different author is allowed");
}

#[tokio::test]
async fn migration_000074_fails_on_duplicate_sequences_and_preserves_all_operations() {
    let url = test_db_url("profile-ops-dup-fail");
    let db = Database::connect(&url).await.expect("connect db");

    // Run migrations up to 73
    Migrator::up(&db, Some(73)).await.expect("migrate to 73");

    let t1 = "2026-09-15T10:00:00Z";
    let t2 = "2026-09-15T10:05:00Z";

    // Insert duplicate sequence operations under migration 73 (non-unique index)
    db.execute_unprepared(&format!(
        "INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-dup-1', 'author-dup', 'site-x', 'display_name', NULL, 'pending', 1, '{t1}', '{t1}'); \
         INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-dup-2', 'author-dup', 'site-x', 'display_name', NULL, 'completed', 1, '{t2}', '{t2}');"
    ))
    .await
    .expect("insert duplicates on migration 73");

    // Attempt to run migration 74 up -> MUST FAIL explicitly
    let mig_res = Migrator::up(&db, Some(1)).await;
    assert!(
        mig_res.is_err(),
        "migration 74 must explicitly fail when duplicate sequences exist"
    );
    let err_msg = mig_res.unwrap_err().to_string();
    assert!(
        err_msg.contains("author-dup")
            && err_msg.contains("site-x")
            && err_msg.contains("display_name"),
        "error message must provide diagnostic details about the duplicate serialization stream: {err_msg}"
    );

    // CRITICAL: BOTH operations MUST still exist in database — nothing silently deleted!
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT operation_id, status FROM profile_operations WHERE author_public_key = 'author-dup' ORDER BY operation_id",
        ))
        .await
        .expect("query rows");

    assert_eq!(
        rows.len(),
        2,
        "migration failure must preserve ALL profile operations without deleting any"
    );
    let id_1: String = rows[0].try_get("", "operation_id").unwrap();
    let id_2: String = rows[1].try_get("", "operation_id").unwrap();
    assert_eq!(id_1, "op-dup-1");
    assert_eq!(id_2, "op-dup-2");
}

#[tokio::test]
async fn migration_000074_succeeds_on_valid_data_and_rollback_is_symmetric() {
    let url = test_db_url("profile-ops-rollback-sym");
    let db = Database::connect(&url).await.expect("connect db");

    // Run migrations up to 73
    Migrator::up(&db, Some(73)).await.expect("migrate to 73");

    let t1 = "2026-09-15T10:00:00Z";
    let t2 = "2026-09-15T10:05:00Z";

    // Insert valid non-duplicate sequences
    db.execute_unprepared(&format!(
        "INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-valid-1', 'author-sym', 'site-x', 'display_name', NULL, 'pending', 1, '{t1}', '{t1}'); \
         INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-valid-2', 'author-sym', 'site-x', 'display_name', NULL, 'pending', 2, '{t2}', '{t2}');"
    ))
    .await
    .expect("insert valid operations on migration 73");

    // Migration 74 up succeeds on valid data
    Migrator::up(&db, Some(1)).await.expect("migrate to 74");

    // Unique constraint is enforced under migration 74
    let dup_res = db
        .execute_unprepared(&format!(
            "INSERT INTO profile_operations \
             (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
             VALUES ('op-valid-3', 'author-sym', 'site-x', 'display_name', NULL, 'pending', 1, '{t2}', '{t2}');"
        ))
        .await;
    assert!(
        dup_res.is_err(),
        "duplicate sequence must be rejected by unique index under migration 74"
    );

    // Rollback migration 74 (down 1 step back to migration 73 state)
    Migrator::down(&db, Some(1))
        .await
        .expect("rollback migration 74");

    // Under migration 73, duplicate sequence is permitted again (restoring non-unique index)
    db.execute_unprepared(&format!(
        "INSERT INTO profile_operations \
         (operation_id, author_public_key, site_id, field, target_value, status, sequence, created_at, updated_at) \
         VALUES ('op-valid-3', 'author-sym', 'site-x', 'display_name', NULL, 'pending', 1, '{t2}', '{t2}');"
    ))
    .await
    .expect("duplicate sequence must succeed after rolling back to migration 73");

    // Re-apply migration 74 -> now it should fail because op-valid-3 created a duplicate!
    let re_up_res = Migrator::up(&db, Some(1)).await;
    assert!(
        re_up_res.is_err(),
        "re-applying migration 74 with duplicate must fail"
    );
}

#[tokio::test]
async fn migration_000075_room_members_media_reference_and_rollback_is_symmetric() {
    let url = test_db_url("migration-000075-media-ref");
    let db = Database::connect(&url).await.expect("connect db");

    // Migrate up to 75
    Migrator::up(&db, Some(75)).await.expect("migrate to 75");

    // Insert a room member with media_reference
    let now = chrono::Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO room_members (room_id, user_id, display_name, avatar_url, media_reference, membership, updated_at) \
         VALUES ('!r:hs', '@u:hs', 'User', 'mxc://hs/pic', 'cumments-media:00000000-0000-4000-8000-000000000001', 'join', '{now}');"
    ))
    .await
    .expect("insert with media_reference under migration 75");

    // Rollback migration 75 (down 1 step)
    Migrator::down(&db, Some(1))
        .await
        .expect("rollback migration 75");

    // Under rolled back state, media_reference column does not exist
    let res = db
        .execute_unprepared(&format!(
            "INSERT INTO room_members (room_id, user_id, display_name, avatar_url, media_reference, membership, updated_at) \
             VALUES ('!r2:hs', '@u2:hs', 'User2', 'mxc://hs/pic2', 'cumments-media:00000000-0000-4000-8000-000000000002', 'join', '{now}');"
        ))
        .await;
    assert!(
        res.is_err(),
        "inserting media_reference must fail after rollback of migration 75"
    );

    // Re-apply migration 75
    Migrator::up(&db, Some(1))
        .await
        .expect("re-apply migration 75");

    // Re-applying adds the column back via alter_table
    db.execute_unprepared(&format!(
        "INSERT INTO room_members (room_id, user_id, display_name, avatar_url, media_reference, membership, updated_at) \
         VALUES ('!r3:hs', '@u3:hs', 'User3', 'mxc://hs/pic3', 'cumments-media:00000000-0000-4000-8000-000000000003', 'join', '{now}');"
    ))
    .await
    .expect("inserting media_reference succeeds after re-applying migration 75");
}

#[tokio::test]
async fn migration_000076_room_members_projection_ordering_and_rollback_is_symmetric() {
    let url = test_db_url("migration-000076-ordering");
    let db = Database::connect(&url).await.expect("connect db");

    // Migrate up to 76
    Migrator::up(&db, Some(76)).await.expect("migrate to 76");

    // Insert a room member with origin_server_ts and event_id
    let now = chrono::Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO room_members (room_id, user_id, display_name, avatar_url, media_reference, membership, origin_server_ts, event_id, updated_at) \
         VALUES ('!r:hs', '@u:hs', 'User', 'mxc://hs/pic', 'cumments-media:00000000-0000-4000-8000-000000000001', 'join', 1000, '$event1', '{now}');"
    ))
    .await
    .expect("insert with ordering fields under migration 76");

    // Rollback migration 76 (down 1 step)
    Migrator::down(&db, Some(1))
        .await
        .expect("rollback migration 76");

    // Under rolled back state, origin_server_ts and event_id do not exist
    let res = db
        .execute_unprepared(&format!(
            "INSERT INTO room_members (room_id, user_id, display_name, avatar_url, media_reference, membership, origin_server_ts, event_id, updated_at) \
             VALUES ('!r2:hs', '@u2:hs', 'User2', 'mxc://hs/pic2', 'cumments-media:00000000-0000-4000-8000-000000000002', 'join', 2000, '$event2', '{now}');"
        ))
        .await;
    assert!(
        res.is_err(),
        "inserting origin_server_ts/event_id must fail after rollback of migration 76"
    );

    // Re-apply migration 76
    Migrator::up(&db, Some(1))
        .await
        .expect("re-apply migration 76");

    // Re-applying adds the columns back
    db.execute_unprepared(&format!(
        "INSERT INTO room_members (room_id, user_id, display_name, avatar_url, media_reference, membership, origin_server_ts, event_id, updated_at) \
         VALUES ('!r3:hs', '@u3:hs', 'User3', 'mxc://hs/pic3', 'cumments-media:00000000-0000-4000-8000-000000000003', 'join', 3000, '$event3', '{now}');"
    ))
    .await
    .expect("inserting ordering columns succeeds after re-applying migration 76");
}

#[tokio::test]
async fn migration_000076_discards_legacy_projection_and_rebuilds_from_canonical() {
    let url = test_db_url("migration-000076-discard-rebuild");
    let db = Database::connect(&url).await.expect("connect db");

    // Migrate up to 75 (before migration 76)
    Migrator::up(&db, Some(75)).await.expect("migrate to 75");

    let t1_str = "2026-09-15T10:00:00.123Z";
    let t1_ms: i64 = 1789466400123;
    let t2_str = "2026-09-15T10:30:00.000Z";
    let t2_ms: i64 = 1789468200000;

    // 1. Canonical Matrix event data in room_state_events
    // User Alice: join event at t1
    db.execute_unprepared(&format!(
        "INSERT INTO room_state_events \
         (event_id, room_id, event_type, state_key, sender, origin_server_ts, content_json, created_at) \
         VALUES ('$join_alice', '!r1:hs', 'm.room.member', '@alice:hs', '@alice:hs', {t1_ms}, \
                 '{{\"membership\":\"join\",\"displayname\":\"Alice\",\"avatar_url\":\"mxc://hs/alice\"}}', '{t1_str}');"
    ))
    .await
    .expect("insert alice state event");

    // User Bob: join at t1, then leave at t2
    db.execute_unprepared(&format!(
        "INSERT INTO room_state_events \
         (event_id, room_id, event_type, state_key, sender, origin_server_ts, content_json, created_at) \
         VALUES ('$join_bob', '!r1:hs', 'm.room.member', '@bob:hs', '@bob:hs', {t1_ms}, \
                 '{{\"membership\":\"join\",\"displayname\":\"Bob\",\"avatar_url\":\"mxc://hs/bob\"}}', '{t1_str}'), \
                ('$leave_bob', '!r1:hs', 'm.room.member', '@bob:hs', '@bob:hs', {t2_ms}, \
                 '{{\"membership\":\"leave\"}}', '{t2_str}');"
    ))
    .await
    .expect("insert bob state events");

    // 2. Pre-076 disposable room_members rows
    db.execute_unprepared(&format!(
        "INSERT INTO room_members (room_id, user_id, display_name, avatar_url, media_reference, membership, updated_at) \
         VALUES ('!r1:hs', '@alice:hs', 'Alice', 'mxc://hs/alice', 'cumments-media:00000000-0000-4000-8000-000000000001', 'join', '{t1_str}'), \
                ('!r1:hs', '@bob:hs', 'Bob', 'mxc://hs/bob', 'cumments-media:00000000-0000-4000-8000-000000000002', 'leave', '{t2_str}');"
    ))
    .await
    .expect("insert legacy room members");

    // Verify pre-conditions: 2 projection rows, 3 canonical state events
    use sea_orm::{ConnectionTrait, Statement};
    let pre_proj_rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT COUNT(*) as cnt FROM room_members".to_string(),
        ))
        .await
        .expect("count pre room members");
    let pre_proj_cnt: i64 = pre_proj_rows[0].try_get("", "cnt").unwrap();
    assert_eq!(pre_proj_cnt, 2);

    let pre_state_rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT COUNT(*) as cnt FROM room_state_events".to_string(),
        ))
        .await
        .expect("count pre room state events");
    let pre_state_cnt: i64 = pre_state_rows[0].try_get("", "cnt").unwrap();
    assert_eq!(pre_state_cnt, 3);

    // 3. Apply migration 76!
    Migrator::up(&db, Some(1))
        .await
        .expect("apply migration 76");

    // Verification 1: room_members projection data is discarded/reset
    let post_proj_rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT COUNT(*) as cnt FROM room_members".to_string(),
        ))
        .await
        .expect("count post room members");
    let post_proj_cnt: i64 = post_proj_rows[0].try_get("", "cnt").unwrap();
    assert_eq!(
        post_proj_cnt, 0,
        "disposable room_members projection must be cleared by migration 76"
    );

    // Verification 2: new columns origin_server_ts and event_id exist
    let columns = column_names(&db, "room_members").await;
    assert!(columns.iter().any(|c| c == "origin_server_ts"));
    assert!(columns.iter().any(|c| c == "event_id"));

    // Verification 3: canonical Matrix event data remains untouched
    let post_state_rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT COUNT(*) as cnt FROM room_state_events".to_string(),
        ))
        .await
        .expect("count post room state events");
    let post_state_cnt: i64 = post_state_rows[0].try_get("", "cnt").unwrap();
    assert_eq!(
        post_state_cnt, 3,
        "canonical room_state_events must remain completely intact"
    );

    // 4. Projection Rebuild from canonical data
    let store = cumments_store::DbStore::connect(&url)
        .await
        .expect("connect store");
    use cumments_core::models::RoomMember;
    use cumments_core::ports::RoomStore;

    // Replay Alice's join
    store
        .save_member(&RoomMember {
            room_id: "!r1:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            display_name: Some("Alice".to_string()),
            avatar_url: Some("mxc://hs/alice".to_string()),
            media_reference: None,
            membership: "join".to_string(),
            origin_server_ts: t1_ms,
            event_id: Some("$join_alice".to_string()),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("replay alice join");

    // Replay Bob's join followed by leave (preserving presentation)
    store
        .save_member(&RoomMember {
            room_id: "!r1:hs".to_string(),
            user_id: "@bob:hs".to_string(),
            display_name: Some("Bob".to_string()),
            avatar_url: Some("mxc://hs/bob".to_string()),
            media_reference: None,
            membership: "join".to_string(),
            origin_server_ts: t1_ms,
            event_id: Some("$join_bob".to_string()),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("replay bob join");
    store
        .save_member(&RoomMember {
            room_id: "!r1:hs".to_string(),
            user_id: "@bob:hs".to_string(),
            display_name: Some("Bob".to_string()),
            avatar_url: Some("mxc://hs/bob".to_string()),
            media_reference: None,
            membership: "leave".to_string(),
            origin_server_ts: t2_ms,
            event_id: Some("$leave_bob".to_string()),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("replay bob leave");

    // Verify repopulated projection rows contain real version metadata
    let alice = store
        .get_member("!r1:hs", "@alice:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.display_name.as_deref(), Some("Alice"));
    assert_eq!(alice.avatar_url.as_deref(), Some("mxc://hs/alice"));
    assert_eq!(alice.membership, "join");
    assert_eq!(alice.origin_server_ts, t1_ms);
    assert_eq!(alice.event_id.as_deref(), Some("$join_alice"));

    let bob = store
        .get_member("!r1:hs", "@bob:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bob.display_name.as_deref(), Some("Bob"));
    assert_eq!(bob.avatar_url.as_deref(), Some("mxc://hs/bob"));
    assert_eq!(bob.membership, "leave");
    assert_eq!(bob.origin_server_ts, t2_ms);
    assert_eq!(bob.event_id.as_deref(), Some("$leave_bob"));

    // 5. Ordering after rebuild:
    // Case A: Older event for Alice (t1_ms - 1000) is rejected
    store
        .save_member(&RoomMember {
            room_id: "!r1:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            display_name: Some("Old Alice".to_string()),
            avatar_url: None,
            media_reference: None,
            membership: "join".to_string(),
            origin_server_ts: t1_ms - 1000,
            event_id: Some("$old_alice".to_string()),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("save older alice");

    let alice_after_older = store
        .get_member("!r1:hs", "@alice:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice_after_older.display_name.as_deref(), Some("Alice"));
    assert_eq!(alice_after_older.origin_server_ts, t1_ms);
    assert_eq!(alice_after_older.event_id.as_deref(), Some("$join_alice"));

    // Case B: Same timestamp with lexicographically smaller event_id is rejected
    store
        .save_member(&RoomMember {
            room_id: "!r1:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            display_name: Some("Smaller Alice".to_string()),
            avatar_url: None,
            media_reference: None,
            membership: "join".to_string(),
            origin_server_ts: t1_ms,
            event_id: Some("$a_alice".to_string()),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("save smaller eid alice");

    let alice_after_smaller = store
        .get_member("!r1:hs", "@alice:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice_after_smaller.display_name.as_deref(), Some("Alice"));
    assert_eq!(alice_after_smaller.event_id.as_deref(), Some("$join_alice"));

    // Case C: Same timestamp with lexicographically larger event_id is accepted
    store
        .save_member(&RoomMember {
            room_id: "!r1:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            display_name: Some("Larger Alice".to_string()),
            avatar_url: Some("mxc://hs/alice-larger".to_string()),
            media_reference: None,
            membership: "join".to_string(),
            origin_server_ts: t1_ms,
            event_id: Some("$z_alice".to_string()),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("save larger eid alice");

    let alice_after_larger = store
        .get_member("!r1:hs", "@alice:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        alice_after_larger.display_name.as_deref(),
        Some("Larger Alice")
    );
    assert_eq!(alice_after_larger.event_id.as_deref(), Some("$z_alice"));

    // Case D: Newer event (t1_ms + 5000) is accepted
    let t_newer = t1_ms + 5000;
    store
        .save_member(&RoomMember {
            room_id: "!r1:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            display_name: Some("Alice Updated".to_string()),
            avatar_url: Some("mxc://hs/alice-updated".to_string()),
            media_reference: None,
            membership: "join".to_string(),
            origin_server_ts: t_newer,
            event_id: Some("$newer_alice".to_string()),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("save newer alice");

    let alice_after_newer = store
        .get_member("!r1:hs", "@alice:hs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        alice_after_newer.display_name.as_deref(),
        Some("Alice Updated")
    );
    assert_eq!(alice_after_newer.origin_server_ts, t_newer);
    assert_eq!(alice_after_newer.event_id.as_deref(), Some("$newer_alice"));
}

#[tokio::test]
async fn messages_table_has_author_media_reference_column() {
    let url = test_db_url("messages-author-media-ref");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, None).await.expect("migrate to latest");

    let cols = column_names(&db, "messages").await;
    assert!(
        cols.contains(&"author_media_reference".to_string()),
        "messages table must include author_media_reference column"
    );
}

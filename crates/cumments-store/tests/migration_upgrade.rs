use chrono::Utc;
use cumments_store::migration::Migrator;
use sea_orm::{ConnectionTrait, Database};
use sea_orm_migration::MigratorTrait;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-migration-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("precreate db file");
    format!("sqlite://{}", path.display())
}

/// Simulate a database upgraded from before migration 000010: apply the first
/// nine migrations, drop the `author_type` column the way the old schema
/// looked, insert legacy rows, then run the remaining migrations.
#[tokio::test]
async fn author_type_backfill_repairs_upgraded_rows() {
    let url = test_db_url("author-type");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, Some(9)).await.expect("migrate to 000009");

    // Emulate the pre-000010 schema (the entity-first migration 000001 has
    // already created `author_type`; drop it so 000010 has to add it back).
    db.execute_unprepared("ALTER TABLE comments DROP COLUMN author_type")
        .await
        .expect("drop author_type");

    let now = Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO comments \
         (event_id, room_id, site_id, page_slug, sender_mxid, author_display_name, \
          content, timestamp, created_at, projected_at, author_public_key, reply_to) \
         VALUES \
         ('$guest', '!room:hs', 'my-blog', 'hello', '', 'Alice', 'hi', '{now}', '{now}', '{now}', 'pk1', NULL), \
         ('$matrix', '!room:hs', 'my-blog', 'hello', '@alice:hs', 'Alice', 'hi', '{now}', '{now}', '{now}', NULL, NULL)"
    ))
    .await
    .expect("insert legacy rows");

    // Run the remaining migrations through 000029 (20 of the 22 pending
    // after the first 9), excluding 000030/000031.
    Migrator::up(&db, Some(20))
        .await
        .expect("migrate to 000029");

    let rows = db
        .query_all_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT event_id, author_type FROM comments \
             WHERE event_id IN ('$guest', '$matrix') ORDER BY event_id",
        ))
        .await
        .expect("query author types");
    let types = rows
        .into_iter()
        .map(|row| {
            let id: String = row.try_get("", "event_id").expect("event_id");
            let ty: String = row.try_get("", "author_type").expect("author_type");
            (id, ty)
        })
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(types.get("$guest"), Some(&"guest".to_string()));
    assert_eq!(types.get("$matrix"), Some(&"matrix".to_string()));

    // The corrective statements must be idempotent.
    db.execute_unprepared(
        "UPDATE comments SET author_type = 'matrix' \
         WHERE author_public_key IS NULL AND author_type = 'guest'",
    )
    .await
    .expect("re-run matrix correction");
    db.execute_unprepared(
        "UPDATE comments SET author_type = 'guest' \
         WHERE author_public_key IS NOT NULL AND author_type = 'matrix'",
    )
    .await
    .expect("re-run guest correction");

    let rows = db
        .query_all_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT event_id, author_type FROM comments ORDER BY event_id",
        ))
        .await
        .expect("query all");
    let types = rows
        .iter()
        .map(|row| {
            let id: String = row.try_get("", "event_id").expect("event_id");
            let ty: String = row.try_get("", "author_type").expect("author_type");
            (id, ty)
        })
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(types.get("$guest"), Some(&"guest".to_string()));
    assert_eq!(types.get("$matrix"), Some(&"matrix".to_string()));
}

#[tokio::test]
async fn comment_updated_at_is_renamed_to_projected_at() {
    let url = test_db_url("projected-at");
    let db = Database::connect(&url).await.expect("connect db");

    // Stop before the rename migration; current entity-first migrations create
    // `projected_at` on fresh databases, so flip it back to the old name to
    // simulate a database created before the rename.
    Migrator::up(&db, Some(28))
        .await
        .expect("migrate to 000028");
    db.execute_unprepared("ALTER TABLE comments RENAME COLUMN projected_at TO updated_at")
        .await
        .expect("simulate pre-rename schema");

    let now = Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO comments \
         (event_id, room_id, site_id, page_slug, sender_mxid, author_display_name, author_type, \
          content, timestamp, created_at, updated_at, author_public_key, reply_to) \
         VALUES \
         ('$guest', '!room:hs', 'my-blog', 'hello', '', 'Alice', 'guest', 'hi', '{now}', '{now}', '{now}', 'pk1', NULL)"
    ))
    .await
    .expect("insert pre-rename row");

    // One pending migration remains after 000028: apply 000029 only.
    Migrator::up(&db, Some(1)).await.expect("migrate to 000029");

    let rows = db
        .query_all_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT projected_at FROM comments WHERE event_id = '$guest'",
        ))
        .await
        .expect("query row");
    assert_eq!(rows.len(), 1, "row must exist after migration");
    let projected_at: String = rows[0].try_get("", "projected_at").expect("projected_at");
    assert_eq!(
        projected_at, now,
        "renamed column must preserve the stored value"
    );
}

#[tokio::test]
async fn message_read_model_replaces_legacy_comments_table() {
    let url = test_db_url("message-read-model");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, None).await.expect("migrate to latest");

    let tables = db
        .query_all_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        ))
        .await
        .expect("list tables");
    let names: Vec<String> = tables
        .into_iter()
        .filter_map(|row| row.try_get("", "name").ok())
        .collect();
    assert!(!names.iter().any(|n| n == "comments"), "comments dropped");
    for expected in [
        "messages",
        "message_revisions",
        "reactions",
        "poll_responses",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing table {expected}: {names:?}"
        );
    }
}

#[tokio::test]
async fn fresh_database_has_no_legacy_blocked_reason_column() {
    let url = test_db_url("fresh-schema");
    let db = Database::connect(&url).await.expect("connect db");
    Migrator::up(&db, None).await.expect("migrate to latest");

    let rows = db
        .query_all_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "PRAGMA table_info(room_registry)",
        ))
        .await
        .expect("read room_registry columns");
    let names: Vec<String> = rows
        .into_iter()
        .filter_map(|row| row.try_get("", "name").ok())
        .collect();
    assert!(
        !names.iter().any(|n| n == "blocked_reason"),
        "fresh schema must not retain the legacy blocked_reason column: {names:?}"
    );
    for expected in [
        "status",
        "quarantine_reason",
        "quarantined_at",
        "adoption_failures",
        "next_attempt_at",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing room_registry column {expected}: {names:?}"
        );
    }
}

#[tokio::test]
async fn room_quarantine_state_backfills_legacy_encoding() {
    let url = test_db_url("quarantine-state");
    let db = Database::connect(&url).await.expect("connect db");

    // Stop before 000030. Entity-first migrations create the *new* columns on
    // fresh databases, so reshape the table back into the legacy
    // `is_active` + `blocked_reason` encoding to simulate an upgraded DB.
    Migrator::up(&db, Some(29))
        .await
        .expect("migrate to 000029");
    db.execute_unprepared("DROP INDEX IF EXISTS idx_room_registry_active_site_post")
        .await
        .expect("drop active index");
    for column in [
        "status",
        "quarantine_reason",
        "blocked_reason",
        "quarantined_at",
        "adoption_failures",
        "next_attempt_at",
    ] {
        db.execute_unprepared(&format!("ALTER TABLE room_registry DROP COLUMN {column}"))
            .await
            .expect("drop new column");
    }
    db.execute_unprepared(
        "ALTER TABLE room_registry ADD COLUMN is_active BOOLEAN NOT NULL DEFAULT 0",
    )
    .await
    .expect("add legacy is_active");
    db.execute_unprepared("ALTER TABLE room_registry ADD COLUMN blocked_reason TEXT")
        .await
        .expect("add legacy blocked_reason");

    let now = Utc::now().to_rfc3339();
    db.execute_unprepared(&format!(
        "INSERT INTO room_registry \
         (room_id, site_id, page_slug, is_active, blocked_reason, created_at, updated_at) \
         VALUES \
         ('!active:hs', 'my-blog', 'active-post', 1, NULL, '{now}', '{now}'), \
         ('!quarantined:hs', 'my-blog', 'quarantined-post', 0, 'Refusing to adopt room', '{now}', '{now}'), \
         ('!superseded:hs', 'my-blog', 'superseded-post', 0, NULL, '{now}', '{now}')"
    ))
    .await
    .expect("insert legacy rows");

    Migrator::up(&db, None).await.expect("migrate to latest");

    // Query the migrated rows and assert the status mapping.
    let rows = db
        .query_all_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT room_id, status, quarantine_reason, adoption_failures, next_attempt_at \
                 FROM room_registry ORDER BY room_id",
        ))
        .await
        .expect("query migrated rows");
    let by_room = rows
        .into_iter()
        .map(|row| {
            let room_id: String = row.try_get("", "room_id").expect("room_id");
            let status: String = row.try_get("", "status").expect("status");
            let quarantine_reason: Option<String> =
                row.try_get("", "quarantine_reason").expect("reason");
            let adoption_failures: i64 = row.try_get("", "adoption_failures").expect("failures");
            (room_id, (status, quarantine_reason, adoption_failures))
        })
        .collect::<std::collections::HashMap<_, _>>();

    assert_eq!(
        by_room.get("!active:hs"),
        Some(&("active".to_string(), None, 0))
    );
    assert_eq!(
        by_room.get("!quarantined:hs"),
        Some(&(
            "quarantined".to_string(),
            Some("Refusing to adopt room".to_string()),
            1
        ))
    );
    assert_eq!(
        by_room.get("!superseded:hs"),
        Some(&("superseded".to_string(), None, 0))
    );
}

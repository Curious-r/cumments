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
          'm20260815_000047_role_claim_dm_room')",
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
    ] {
        db.execute_unprepared(sql).await.expect("rewind schema");
    }

    Migrator::up(&db, None).await.expect("upgrade migrate");

    let post_columns = column_names(&db, "post_submissions").await;
    let delete_columns = column_names(&db, "delete_submissions").await;
    let update_columns = column_names(&db, "update_submissions").await;
    let claim_columns = column_names(&db, "role_claims").await;

    assert!(post_columns.iter().any(|c| c == "txn_id"));
    assert!(!post_columns.iter().any(|c| c == "force_new_txn"));
    for columns in [&delete_columns, &update_columns] {
        assert!(columns.iter().any(|c| c == "txn_id"));
        assert!(columns.iter().any(|c| c == "matrix_event_id"));
    }
    assert!(claim_columns.iter().any(|c| c == "dm_room_id"));
}

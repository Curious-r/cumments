use chrono::Utc;
use cumments_store::entities::comments;
use cumments_store::migration::Migrator;
use sea_orm::{ColumnTrait, ConnectionTrait, Database, EntityTrait, QueryFilter};
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
         (event_id, room_id, site_id, post_slug, sender_mxid, author_display_name, \
          content, timestamp, created_at, updated_at, author_public_key, reply_to) \
         VALUES \
         ('$guest', '!room:hs', 'my-blog', 'hello', '', 'Alice', 'hi', '{now}', '{now}', '{now}', 'pk1', NULL), \
         ('$matrix', '!room:hs', 'my-blog', 'hello', '@alice:hs', 'Alice', 'hi', '{now}', '{now}', '{now}', NULL, NULL)"
    ))
    .await
    .expect("insert legacy rows");

    // Run the remaining migrations (000010..000017).
    Migrator::up(&db, None).await.expect("migrate to latest");

    let rows = comments::Entity::find()
        .filter(comments::Column::EventId.eq("$guest"))
        .all(&db)
        .await
        .expect("query guest");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].author_type, "guest");

    let rows = comments::Entity::find()
        .filter(comments::Column::EventId.eq("$matrix"))
        .all(&db)
        .await
        .expect("query matrix");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].author_type, "matrix");

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

    let rows = comments::Entity::find().all(&db).await.expect("query all");
    let types = rows
        .iter()
        .map(|row| (row.event_id.as_str(), row.author_type.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(types.get("$guest"), Some(&"guest"));
    assert_eq!(types.get("$matrix"), Some(&"matrix"));
}

use sea_orm::Statement;
use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "media_uploads";
const COLUMN: &str = "post_slug";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Make `media_uploads.post_slug` nullable so site-scoped identity media
/// (guest avatars) shares the same ownership/idempotency machinery as
/// comment-scoped uploads without pretending to belong to a post.
///
/// SQLite cannot alter a column's nullability, so the table is rebuilt with
/// the new shape and the existing rows are copied verbatim.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_nullable(manager).await? {
            return Ok(());
        }
        rebuild(manager, true).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_nullable(manager).await? {
            return Ok(());
        }
        rebuild(manager, false).await
    }
}

async fn column_nullable(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    if !column_exists(manager, TABLE, COLUMN).await? {
        return Ok(true);
    }
    let db = manager.get_connection();
    let sql = format!("PRAGMA table_info({TABLE})");
    let rows = db
        .query_all_raw(Statement::from_string(manager.get_database_backend(), sql))
        .await?;
    for row in rows {
        if row.try_get::<String>("", "name")? == COLUMN {
            let not_null: i64 = row.try_get("", "notnull")?;
            return Ok(not_null == 0);
        }
    }
    Ok(true)
}

async fn rebuild(manager: &SchemaManager<'_>, nullable: bool) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let post_slug_type = if nullable { "TEXT" } else { "TEXT NOT NULL" };
    let temp = "media_uploads_new";

    db.execute_unprepared(&format!("DROP TABLE IF EXISTS {temp}"))
        .await?;
    db.execute_unprepared(&format!(
        "CREATE TABLE {temp} (
            id INTEGER PRIMARY KEY,
            mxc_url TEXT NOT NULL UNIQUE,
            author_public_key TEXT NOT NULL,
            site_id TEXT NOT NULL,
            post_slug {post_slug_type},
            used_at TEXT,
            submission_id INTEGER,
            created_at TEXT NOT NULL
        )"
    ))
    .await?;
    db.execute_unprepared(&format!(
        "INSERT INTO {temp} (id, mxc_url, author_public_key, site_id, post_slug, used_at, submission_id, created_at)
         SELECT id, mxc_url, author_public_key, site_id, COALESCE(post_slug, ''), used_at, submission_id, created_at
         FROM {TABLE}"
    ))
    .await?;
    db.execute_unprepared(&format!("DROP TABLE {TABLE}"))
        .await?;
    db.execute_unprepared(&format!("ALTER TABLE {temp} RENAME TO {TABLE}"))
        .await?;
    db.execute_unprepared(
        "CREATE INDEX idx_media_uploads_author ON media_uploads (author_public_key)",
    )
    .await?;
    Ok(())
}

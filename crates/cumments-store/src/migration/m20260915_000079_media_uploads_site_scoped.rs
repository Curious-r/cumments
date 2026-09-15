use sea_orm_migration::prelude::*;

const TABLE: &str = "media_uploads";
const TEMP: &str = "media_uploads_new";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Scope media upload ownership to `(site_id, mxc_url)`.
///
/// `mxc_url` stops being globally unique so the same MXC URI may have one
/// ownership row per site. SQLite cannot drop a column-level `UNIQUE`, so the
/// table is rebuilt with the new constraint and existing rows are copied
/// verbatim: ids, sites, and every other column are preserved exactly.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        rebuild(manager, true).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        rebuild(manager, false).await
    }
}

async fn rebuild(manager: &SchemaManager<'_>, site_scoped: bool) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let mxc_column = if site_scoped {
        "mxc_url TEXT NOT NULL"
    } else {
        "mxc_url TEXT NOT NULL UNIQUE"
    };
    let uniqueness = if site_scoped {
        ",\n            UNIQUE(site_id, mxc_url)"
    } else {
        ""
    };

    db.execute_unprepared(&format!("DROP TABLE IF EXISTS {TEMP}"))
        .await?;
    // `AUTOINCREMENT` keeps row ids monotonic so a released id is never reused
    // by a later replacement row, which compare-and-release relies on.
    db.execute_unprepared(&format!(
        "CREATE TABLE {TEMP} (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            {mxc_column},
            author_public_key TEXT NOT NULL,
            site_id TEXT NOT NULL,
            page_slug TEXT,
            used_at TEXT,
            submission_id INTEGER,
            created_at TEXT NOT NULL{uniqueness}
        )"
    ))
    .await?;
    db.execute_unprepared(&format!(
        "INSERT INTO {TEMP} (id, mxc_url, author_public_key, site_id, page_slug, used_at, submission_id, created_at)
         SELECT id, mxc_url, author_public_key, site_id, page_slug, used_at, submission_id, created_at
         FROM {TABLE}"
    ))
    .await?;
    db.execute_unprepared(&format!("DROP TABLE {TABLE}"))
        .await?;
    db.execute_unprepared(&format!("ALTER TABLE {TEMP} RENAME TO {TABLE}"))
        .await?;
    db.execute_unprepared(
        "CREATE INDEX idx_media_uploads_author ON media_uploads (author_public_key)",
    )
    .await?;
    Ok(())
}

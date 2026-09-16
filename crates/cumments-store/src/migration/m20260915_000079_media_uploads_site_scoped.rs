use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

const TABLE: &str = "media_uploads";
const TEMP: &str = "media_uploads_new";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Scope media upload ownership to `(site_id, mxc_url)`.
///
/// `mxc_url` stops being globally unique so the same MXC URI may have one
/// ownership row per site. SQLite cannot drop a column-level `UNIQUE`, so the
/// table is rebuilt and existing rows are copied verbatim: ids, sites, and
/// every other column are preserved exactly.
///
/// The only schema object being replaced is the old `UNIQUE(mxc_url)`. Every
/// other explicit object on the table (indexes, triggers) is captured before
/// the swap and recreated afterwards; auto indexes are recreated by the new
/// table definition.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        rebuild(manager, true).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        rebuild(manager, false).await
    }
}

/// SQL of the table's explicit indexes and triggers, which SQLite drops along
/// with the table and which must therefore be recreated after the rebuild.
async fn dependent_objects<C: ConnectionTrait>(db: &C, kind: &str) -> Result<Vec<String>, DbErr> {
    let sql = format!(
        "SELECT sql FROM sqlite_master \
         WHERE type = '{kind}' AND tbl_name = '{TABLE}' AND sql IS NOT NULL"
    );
    let rows = db
        .query_all_raw(Statement::from_string(db.get_database_backend(), sql))
        .await?;
    let mut statements = Vec::with_capacity(rows.len());
    for row in rows {
        statements.push(row.try_get::<String>("", "sql")?);
    }
    Ok(statements)
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

    // Capture unrelated schema objects before dropping the table. Auto indexes
    // (`sqlite_autoindex_*`) have no SQL and are recreated by the CREATE TABLE.
    let mut dependents = dependent_objects(db, "index").await?;
    dependents.extend(dependent_objects(db, "trigger").await?);

    db.execute_unprepared(&format!("DROP TABLE IF EXISTS {TEMP}"))
        .await?;
    // `AUTOINCREMENT` keeps row ids monotonic across deletes.
    db.execute_unprepared(&format!(
        "CREATE TABLE {TEMP} (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            {mxc_column},
            author_public_key TEXT NOT NULL,
            site_id TEXT NOT NULL,
            page_slug TEXT,
            created_at TEXT NOT NULL{uniqueness}
        )"
    ))
    .await?;
    db.execute_unprepared(&format!(
        "INSERT INTO {TEMP} (id, mxc_url, author_public_key, site_id, page_slug, created_at)
         SELECT id, mxc_url, author_public_key, site_id, page_slug, created_at
         FROM {TABLE}"
    ))
    .await?;
    db.execute_unprepared(&format!("DROP TABLE {TABLE}"))
        .await?;
    db.execute_unprepared(&format!("ALTER TABLE {TEMP} RENAME TO {TABLE}"))
        .await?;

    for statement in dependents {
        db.execute_unprepared(&statement).await?;
    }
    Ok(())
}

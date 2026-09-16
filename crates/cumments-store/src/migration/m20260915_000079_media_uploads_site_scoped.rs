use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

const TABLE: &str = "media_uploads";
const TEMP: &str = "media_uploads_new";
/// Partial unique index for page-scoped (comment) upload provenance.
const COMMENT_UNIQUE_INDEX: &str = "media_uploads_comment_scope_unique";
/// Partial unique index for site-scoped (avatar) upload provenance.
const AVATAR_UNIQUE_INDEX: &str = "media_uploads_avatar_scope_unique";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Define the upload-provenance uniqueness of `media_uploads`.
///
/// A row is an authorization fact `(site, visitor, scope, mxc)`, where the
/// scope is the page for comment media and "no page" for a site-scoped avatar:
///
/// ```text
/// comment media: site + visitor + page + mxc
/// avatar media:  site + visitor + mxc        (page_slug IS NULL)
/// ```
///
/// The unique key must cover the whole fact, because Matrix does not guarantee
/// that an MXC URI originates from exactly one upload transaction. The
/// specification is silent on upload deduplication, so a homeserver may return
/// an existing `mxc://server/media_id` for bytes it already holds. The same MXC
/// can therefore legitimately appear in provenance records for different
/// visitors or different scopes, and those records must coexist.
///
/// Two partial unique indexes express the two scopes exactly:
///
/// - comment scope: `(site_id, author_public_key, mxc_url, page_slug)` where
///   `page_slug IS NOT NULL`
/// - avatar scope: `(site_id, author_public_key, mxc_url)` where
///   `page_slug IS NULL`
///
/// A column-level `UNIQUE` cannot express this: SQLite treats `NULL` values as
/// distinct, so a nullable `page_slug` would not deduplicate avatar rows. A
/// conflict on either index therefore means the exact same provenance fact was
/// recorded twice, and never that one visitor's record collides with another's.
///
/// SQLite cannot drop a column-level `UNIQUE`, so the table is rebuilt and
/// existing rows are copied verbatim: ids, sites, and every other column are
/// preserved exactly. Every other explicit object on the table (indexes,
/// triggers) is captured before the swap and recreated afterwards.
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

async fn rebuild(manager: &SchemaManager<'_>, provenance_scoped: bool) -> Result<(), DbErr> {
    let db = manager.get_connection();
    // The legacy (pre-000079) shape kept the MXC globally unique; the
    // provenance shape carries no inline uniqueness and is indexed by scope.
    let mxc_column = if provenance_scoped {
        "mxc_url TEXT NOT NULL"
    } else {
        "mxc_url TEXT NOT NULL UNIQUE"
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
            created_at TEXT NOT NULL
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

    if provenance_scoped {
        // Comment scope: a page-scoped upload by this visitor for this MXC.
        db.execute_unprepared(&format!(
            "CREATE UNIQUE INDEX {COMMENT_UNIQUE_INDEX} \
             ON {TABLE} (site_id, author_public_key, mxc_url, page_slug) \
             WHERE page_slug IS NOT NULL"
        ))
        .await?;
        // Avatar scope: a site-scoped upload by this visitor for this MXC.
        db.execute_unprepared(&format!(
            "CREATE UNIQUE INDEX {AVATAR_UNIQUE_INDEX} \
             ON {TABLE} (site_id, author_public_key, mxc_url) \
             WHERE page_slug IS NULL"
        ))
        .await?;
    }
    Ok(())
}

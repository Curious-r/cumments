use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

/// Tables that carry the `post_slug` column at this point in the migration
/// chain. `comments` is included defensively for databases that were paused
/// before the read-model migration dropped it.
const POST_SLUG_TABLES: &[&str] = &[
    "comments",
    "messages",
    "room_registry",
    "media_uploads",
    "update_submissions",
];

/// (table, column) pairs whose stored author-kind values still use the old
/// `guest` vocabulary. The legacy `comments` table is included defensively;
/// the current read model stores the kind on `messages.author_kind`.
const AUTHOR_KIND_COLUMNS: &[(&str, &str)] =
    &[("comments", "author_type"), ("messages", "author_kind")];

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Terminology rename (Phase A): `post_slug` becomes `page_slug` and the
/// visitor author type value becomes `visitor`. Historical migrations keep
/// the old vocabulary; this migration converges existing databases to the
/// current terms.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in POST_SLUG_TABLES {
            rename_column(manager, table, "post_slug", "page_slug").await?;
        }
        for (table, column) in AUTHOR_KIND_COLUMNS {
            rewrite_author_kind(manager, table, column, "guest", "visitor").await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in POST_SLUG_TABLES {
            rename_column(manager, table, "page_slug", "post_slug").await?;
        }
        for (table, column) in AUTHOR_KIND_COLUMNS {
            rewrite_author_kind(manager, table, column, "visitor", "guest").await?;
        }
        Ok(())
    }
}

async fn rename_column(
    manager: &SchemaManager<'_>,
    table: &str,
    old: &str,
    new: &str,
) -> Result<(), DbErr> {
    // Table/column names come from internal constants, so direct
    // interpolation is safe.
    if column_exists(manager, table, old).await? && !column_exists(manager, table, new).await? {
        let db = manager.get_connection();
        db.execute_unprepared(&format!("ALTER TABLE {table} RENAME COLUMN {old} TO {new}"))
            .await?;
    }
    Ok(())
}

async fn rewrite_author_kind(
    manager: &SchemaManager<'_>,
    table: &str,
    column: &str,
    old: &str,
    new: &str,
) -> Result<(), DbErr> {
    if column_exists(manager, table, column).await? {
        let db = manager.get_connection();
        db.execute_unprepared(&format!(
            "UPDATE {table} SET {column} = '{new}' WHERE {column} = '{old}'"
        ))
        .await?;
    }
    Ok(())
}

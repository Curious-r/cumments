use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "comments";
const OLD_COLUMN: &str = "updated_at";
const NEW_COLUMN: &str = "projected_at";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Renames `comments.updated_at` to `comments.projected_at`.
///
/// The old name invites confusion with the comment's Matrix edit time;
/// `projected_at` states plainly that it is the local read-model write time.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, TABLE, OLD_COLUMN).await?
            && !column_exists(manager, TABLE, NEW_COLUMN).await?
        {
            manager
                .get_connection()
                .execute_unprepared(&format!(
                    "ALTER TABLE {TABLE} RENAME COLUMN {OLD_COLUMN} TO {NEW_COLUMN}"
                ))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, TABLE, NEW_COLUMN).await?
            && !column_exists(manager, TABLE, OLD_COLUMN).await?
        {
            manager
                .get_connection()
                .execute_unprepared(&format!(
                    "ALTER TABLE {TABLE} RENAME COLUMN {NEW_COLUMN} TO {OLD_COLUMN}"
                ))
                .await?;
        }
        Ok(())
    }
}

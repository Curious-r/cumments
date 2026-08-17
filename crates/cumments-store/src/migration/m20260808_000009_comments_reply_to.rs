use sea_orm_migration::prelude::*;

use crate::migration::{column_exists, slug_column};

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add the reply-tree parent pointer to the comments read model.
///
/// Fresh databases already get this column from the current entity via
/// migration 000001, so the column addition must be idempotent.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let slug = slug_column(manager, "comments").await?;
        if !column_exists(manager, "comments", "reply_to").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .add_column(ColumnDef::new(Alias::new("reply_to")).string().null())
                        .to_owned(),
                )
                .await?;
        }

        manager
            .get_connection()
            .execute_unprepared(&format!(
                "CREATE INDEX IF NOT EXISTS idx_comments_site_post_reply \
                     ON comments (site_id, {slug}, reply_to)"
            ))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_comments_site_post_reply")
            .await?;

        if column_exists(manager, "comments", "reply_to").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .drop_column(Alias::new("reply_to"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

use sea_orm_migration::prelude::*;

/// Rename `backfill_cursors.next_batch` to `next_token`: the column stores a
/// `/messages` pagination token, not a sync batch token.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "backfill_cursors", "next_batch").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("backfill_cursors"))
                        .rename_column(Alias::new("next_batch"), Alias::new("next_token"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "backfill_cursors", "next_token").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("backfill_cursors"))
                        .rename_column(Alias::new("next_token"), Alias::new("next_batch"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

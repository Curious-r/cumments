use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "intent_queue_post_comment";
const COLUMN: &str = "last_timeout_confirmation_at";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Records when the last timeout confirmation was observed so a confirmation
/// pass is genuinely separated in time instead of being consumed three times
/// inside the same reconcile loop.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .add_column(ColumnDef::new(Alias::new(COLUMN)).big_integer().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .drop_column(Alias::new(COLUMN))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

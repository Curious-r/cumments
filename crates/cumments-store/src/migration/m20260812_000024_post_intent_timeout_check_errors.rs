use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "intent_queue_post_comment";
const COLUMN: &str = "timeout_check_errors";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Track consecutive event-existence check failures so timeout passes can
/// dead-letter intents after repeated errors instead of retrying forever.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .add_column(
                            ColumnDef::new(Alias::new(COLUMN))
                                .integer()
                                .not_null()
                                .default(0),
                        )
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

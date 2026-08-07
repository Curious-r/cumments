use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add retry bookkeeping columns to all three intent queues:
/// `next_attempt_at`, `last_error`, and (for delete/update) `retry_count`.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(intent_queue_post_comment::Entity)
                    .add_column(
                        ColumnDef::new(Alias::new("next_attempt_at"))
                            .date_time()
                            .null(),
                    )
                    .add_column(ColumnDef::new(Alias::new("last_error")).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(intent_queue_delete_comment::Entity)
                    .add_column(
                        ColumnDef::new(Alias::new("next_attempt_at"))
                            .date_time()
                            .null(),
                    )
                    .add_column(ColumnDef::new(Alias::new("last_error")).string().null())
                    .add_column(
                        ColumnDef::new(Alias::new("retry_count"))
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(intent_queue_update_comment::Entity)
                    .add_column(
                        ColumnDef::new(Alias::new("next_attempt_at"))
                            .date_time()
                            .null(),
                    )
                    .add_column(ColumnDef::new(Alias::new("last_error")).string().null())
                    .add_column(
                        ColumnDef::new(Alias::new("retry_count"))
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(intent_queue_post_comment::Entity)
                    .drop_column(Alias::new("next_attempt_at"))
                    .drop_column(Alias::new("last_error"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(intent_queue_delete_comment::Entity)
                    .drop_column(Alias::new("next_attempt_at"))
                    .drop_column(Alias::new("last_error"))
                    .drop_column(Alias::new("retry_count"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(intent_queue_update_comment::Entity)
                    .drop_column(Alias::new("next_attempt_at"))
                    .drop_column(Alias::new("last_error"))
                    .drop_column(Alias::new("retry_count"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

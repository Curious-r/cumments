use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add retry bookkeeping columns to all three intent queues:
/// `next_attempt_at`, `last_error`, and (for delete/update) `retry_count`.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "intent_queue_post_comment",
            "intent_queue_delete_comment",
            "intent_queue_update_comment",
        ] {
            if !column_exists(manager, table, "next_attempt_at").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(
                                ColumnDef::new(Alias::new("next_attempt_at"))
                                    .date_time()
                                    .null(),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
            if !column_exists(manager, table, "last_error").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(ColumnDef::new(Alias::new("last_error")).string().null())
                            .to_owned(),
                    )
                    .await?;
            }
        }

        for table in ["intent_queue_delete_comment", "intent_queue_update_comment"] {
            if !column_exists(manager, table, "retry_count").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(
                                ColumnDef::new(Alias::new("retry_count"))
                                    .integer()
                                    .not_null()
                                    .default(0),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "intent_queue_post_comment",
            "intent_queue_delete_comment",
            "intent_queue_update_comment",
        ] {
            for column in ["next_attempt_at", "last_error"] {
                if column_exists(manager, table, column).await? {
                    manager
                        .alter_table(
                            Table::alter()
                                .table(Alias::new(table))
                                .drop_column(Alias::new(column))
                                .to_owned(),
                        )
                        .await?;
                }
            }
        }
        for table in ["intent_queue_delete_comment", "intent_queue_update_comment"] {
            if column_exists(manager, table, "retry_count").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .drop_column(Alias::new("retry_count"))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}

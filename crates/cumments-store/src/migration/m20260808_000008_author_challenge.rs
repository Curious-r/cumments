use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Store the PoW challenge prefix on queued updates so the reconciler can
/// publish it with the Matrix edit event, keeping the Ed25519 signature
/// independently verifiable from the event log.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, "intent_queue_update_comment", "author_challenge").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("intent_queue_update_comment"))
                        .add_column(
                            ColumnDef::new(Alias::new("author_challenge"))
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, "intent_queue_update_comment", "author_challenge").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("intent_queue_update_comment"))
                        .drop_column(Alias::new("author_challenge"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Persist acknowledged AppService transaction IDs so a process restart cannot
/// turn a homeserver retry into duplicate SSE broadcasts. The bounded table is
/// only a fast-path dedupe; immutable event projections remain authoritative.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(
                schema.create_table_from_entity(processed_appservice_transactions::Entity),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_appservice_txn_processed_at")
                    .table(processed_appservice_transactions::Entity)
                    .col(processed_appservice_transactions::Column::ProcessedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(processed_appservice_transactions::Entity)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

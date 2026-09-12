use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Transport execution state for claimed operations.
///
/// Persists the downstream Matrix `txn_id` and delivery status separately
/// from the operation claim identity. Even if an operation has no durable
/// submission row (e.g. Vote), it persists its transaction ID here prior to
/// network dispatch so retries reuse the exact same `txn_id`.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(schema.create_table_from_entity(operation_executions::Entity))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(operation_executions::Entity).to_owned())
            .await?;
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Server-wide logical operation identity.
///
/// The unique index on `operation_id` is what makes an operation claim atomic:
/// two concurrent requests cannot both insert the same key, so only one
/// logical operation is ever claimed (frozen Poll design §5.1). This is a new,
/// additive table; the author-scoped `idempotency_keys` table is deliberately
/// left untouched so existing comment behavior is preserved.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(schema.create_table_from_entity(operation_claims::Entity))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(operation_claims::Entity).to_owned())
            .await?;
        Ok(())
    }
}

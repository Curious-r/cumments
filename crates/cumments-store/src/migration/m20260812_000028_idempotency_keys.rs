use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Idempotency bookkeeping for comment write intents.
///
/// One row per `(author_public_key, Idempotency-Key)` pair; the unique index
/// is what makes concurrent duplicate submissions fail atomically instead of
/// queueing two intents.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(schema.create_table_from_entity(idempotency_keys::Entity))
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("uniq_idempotency_author_key")
                    .table(idempotency_keys::Entity)
                    .col(idempotency_keys::Column::AuthorPublicKey)
                    .col(idempotency_keys::Column::IdempotencyKey)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_idempotency_created_at")
                    .table(idempotency_keys::Entity)
                    .col(idempotency_keys::Column::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(idempotency_keys::Entity).to_owned())
            .await?;
        Ok(())
    }
}

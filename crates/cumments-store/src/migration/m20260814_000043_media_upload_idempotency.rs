use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Idempotency records for guest media uploads. Kept separate from
/// `media_uploads` so an ownership row stays stable once a comment references
/// it, while the key may be reused after the 24-hour retention window.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(media_upload_idempotency::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("uq_media_upload_idempotency_key")
                    .table(media_upload_idempotency::Entity)
                    .col(media_upload_idempotency::Column::AuthorPublicKey)
                    .col(media_upload_idempotency::Column::IdempotencyKey)
                    .unique()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(media_upload_idempotency::Entity)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

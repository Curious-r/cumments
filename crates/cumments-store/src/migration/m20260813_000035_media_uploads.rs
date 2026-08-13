use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Track guest uploads so comment intents can only reference media uploaded
/// by the same author for the same site/post, and so orphan cleanup can find
/// media that was never referenced.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(media_uploads::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_media_uploads_author")
                    .table(media_uploads::Entity)
                    .col(media_uploads::Column::AuthorPublicKey)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(media_uploads::Entity).to_owned())
            .await?;
        Ok(())
    }
}

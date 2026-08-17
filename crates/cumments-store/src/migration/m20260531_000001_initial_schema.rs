use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(sites::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(comments::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(room_registry::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(post_submissions::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(delete_submissions::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(update_submissions::Entity))
            .await?;

        // Manual indexes not captured by basic entity derivation: composite
        // indexes must be added explicitly even though SeaORM derives unique
        // and primary keys from the entity attributes.

        manager
            .create_index(
                Index::create()
                    .name("idx_comments_site_post")
                    .table(comments::Entity)
                    .col(comments::Column::SiteId)
                    .col(comments::Column::PageSlug)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_room_registry_site_post")
                    .table(room_registry::Entity)
                    .col(room_registry::Column::SiteId)
                    .col(room_registry::Column::PageSlug)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(update_submissions::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(delete_submissions::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(post_submissions::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(room_registry::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(comments::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(sites::Entity).to_owned())
            .await?;

        Ok(())
    }
}

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
            .create_table(schema.create_table_from_entity(intent_queue_post_comment::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(intent_queue_delete_comment::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(intent_queue_update_comment::Entity))
            .await?;

        // Manual indexes not captured by basic entity derivation (if any)
        // SeaORM 1.x captures unique and primary keys, but composite indexes might need manual addition
        // if not specified in the Entity via attributes.

        manager
            .create_index(
                Index::create()
                    .name("idx_comments_site_post")
                    .table(comments::Entity)
                    .col(comments::Column::SiteId)
                    .col(comments::Column::PostSlug)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_room_registry_site_post")
                    .table(room_registry::Entity)
                    .col(room_registry::Column::SiteId)
                    .col(room_registry::Column::PostSlug)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(intent_queue_update_comment::Entity)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(intent_queue_delete_comment::Entity)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(intent_queue_post_comment::Entity)
                    .to_owned(),
            )
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

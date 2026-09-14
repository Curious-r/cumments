use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Durable site-scoped mapping between a stable MediaReference
/// and an underlying Matrix homeserver MXC URI.
///
/// Uniqueness is strictly site-scoped: UNIQUE(site_id, mxc_uri).
/// MXC URIs are deliberately not globally unique across sites.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(media_references::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("uq_media_references_site_mxc")
                    .table(media_references::Entity)
                    .col(media_references::Column::SiteId)
                    .col(media_references::Column::MxcUri)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_media_references_site")
                    .table(media_references::Entity)
                    .col(media_references::Column::SiteId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(media_references::Entity).to_owned())
            .await?;
        Ok(())
    }
}

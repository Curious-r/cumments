use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Process-state table backing site ownership transfers.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(site_transfers::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_site_transfers_site_status")
                    .table(site_transfers::Entity)
                    .col(site_transfers::Column::SiteId)
                    .col(site_transfers::Column::Status)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(site_transfers::Entity).to_owned())
            .await?;
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Projection tables for site and room governance roles. They are disposable
/// read-model caches; Matrix power levels are the source of truth.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(site_roles::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(room_roles::Entity))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(room_roles::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(site_roles::Entity).to_owned())
            .await?;
        Ok(())
    }
}

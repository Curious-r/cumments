use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Persists facts that failed closed (starting with unsupported room-version
/// state redactions) so they remain visible and repairable after restarts.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(schema.create_table_from_entity(projection_repairs::Entity))
            .await?;
        manager
            .create_index(
                sea_orm_migration::prelude::Index::create()
                    .name("idx_projection_repairs_due")
                    .table(projection_repairs::Entity)
                    .col(projection_repairs::Column::NextRetryAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(projection_repairs::Entity).to_owned())
            .await
    }
}

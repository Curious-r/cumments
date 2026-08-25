use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = sea_orm::Schema::new(manager.get_database_backend());
        manager
            .create_table(schema.create_table_from_entity(sse_outbox::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_sse_outbox_pending")
                    .table(sse_outbox::Entity)
                    .col(sse_outbox::Column::Id)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(sse_outbox::Entity).to_owned())
            .await
    }
}

use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = sea_orm::Schema::new(manager.get_database_backend());
        manager
            .create_table(schema.create_table_from_entity(room_upgrade_intents::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_room_upgrade_intents_status")
                    .table(room_upgrade_intents::Entity)
                    .col(room_upgrade_intents::Column::Status)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(room_upgrade_intents::Entity).to_owned())
            .await
    }
}

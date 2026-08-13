use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Room-level metadata: member profiles and the system-message (state event)
/// feed, independent from the message read model.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(room_members::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(room_state_events::Entity))
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_room_members_room")
                    .table(room_members::Entity)
                    .col(room_members::Column::RoomId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_room_state_room_ts")
                    .table(room_state_events::Entity)
                    .col(room_state_events::Column::RoomId)
                    .col(room_state_events::Column::OriginServerTs)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(room_state_events::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(room_members::Entity).to_owned())
            .await?;
        Ok(())
    }
}

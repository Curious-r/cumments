use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Store every `org.matrix.msc3381.poll.end` event as an immutable relation
/// fact so the effective end can be derived deterministically and redaction
/// can revert to a later surviving end.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(schema.create_table_from_entity(poll_end_events::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_poll_end_events_poll")
                    .table(poll_end_events::Entity)
                    .col(poll_end_events::Column::PollMessageId)
                    .col(poll_end_events::Column::OriginServerTs)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(poll_end_events::Entity).to_owned())
            .await?;
        Ok(())
    }
}

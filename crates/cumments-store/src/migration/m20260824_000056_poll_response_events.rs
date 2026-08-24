use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Store every poll response as an immutable relation fact. The former
/// per-voter table collapsed history, so redacting a newer vote could not
/// restore the previous choice.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(schema.create_table_from_entity(poll_response_events::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_poll_response_events_voter")
                    .table(poll_response_events::Entity)
                    .col(poll_response_events::Column::PollMessageId)
                    .col(poll_response_events::Column::SenderMxid)
                    .to_owned(),
            )
            .await?;

        // Legacy rows after 000034 carry their event IDs. Rows without IDs
        // cannot participate in redaction and were already collapsed by 0036;
        // they remain in the legacy table for audit but cannot be imported as
        // relation facts.
        manager
            .get_connection()
            .execute_unprepared(
                r"INSERT OR IGNORE INTO poll_response_events
                  (event_id, poll_message_id, sender_mxid, option_index, origin_server_ts,
                   redacted_at, redacted_by, created_at)
                  SELECT event_id, poll_message_id, sender_mxid, option_index, origin_server_ts,
                         redacted_at, redacted_by, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                  FROM poll_responses WHERE event_id IS NOT NULL",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(poll_response_events::Entity).to_owned())
            .await?;
        Ok(())
    }
}

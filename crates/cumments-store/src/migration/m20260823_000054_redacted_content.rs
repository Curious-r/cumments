use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Sanitize redacted comments already present in live read models.
///
/// Ordinary backfill cannot repair these rows: once a tombstone exists, the
/// original event is deliberately skipped to prevent resurrection.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"UPDATE messages
              SET content_json = '{"type":"redacted"}',
                  raw_content_json = '{}',
                  last_edit_ts = NULL,
                  last_edit_event_id = NULL,
                  reply_to = NULL,
                  thread_root = NULL,
                  submission_id = NULL,
                  updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE status = 'redacted'"#,
        )
        .await?;
        db.execute_unprepared(
            r"DELETE FROM message_revisions
              WHERE message_event_id IN (
                  SELECT event_id FROM messages WHERE status = 'redacted'
              )",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Original redacted content was intentionally destroyed and cannot be
        // reconstructed from the read model.
        Ok(())
    }
}

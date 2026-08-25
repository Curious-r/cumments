use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Removes authored payloads that survived the earlier redaction schema
/// changes: originals retained by active rows that were later deleted, and
/// replacement bodies on individually redacted edits.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"UPDATE messages
              SET original_content_json = '{"type":"redacted"}'
              WHERE status = 'redacted'"#,
        )
        .await?;
        db.execute_unprepared(
            r#"UPDATE message_revisions
              SET content_json = '{"type":"redacted"}'
              WHERE redacted_at IS NOT NULL
                 OR message_event_id IN (
                     SELECT event_id FROM messages WHERE status = 'redacted'
                 )"#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Redacted authored payloads were intentionally destroyed and cannot be
        // reconstructed from the read model.
        Ok(())
    }
}

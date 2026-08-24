use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Preserve the pre-edit payload and record enough relation metadata to let a
/// redacted replacement roll the public view back deterministically.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Entity-first migrations create fresh databases from the current
        // models, so these columns may already exist before this step runs.
        let messages_columns_added =
            !crate::migration::column_exists(manager, "messages", "original_content_json").await?;
        if messages_columns_added {
            db.execute_unprepared(
                r"ALTER TABLE messages ADD COLUMN original_content_json TEXT NOT NULL DEFAULT '{}'",
            )
            .await?;
        }
        if !crate::migration::column_exists(manager, "messages", "matrix_event_type").await? {
            db.execute_unprepared(
                r"ALTER TABLE messages ADD COLUMN matrix_event_type TEXT NOT NULL DEFAULT 'm.room.message'",
            )
            .await?;
        }
        if messages_columns_added {
            // Existing rows may already contain an edited current payload.
            // Without production history to migrate, preserve what is visible
            // today; full backfill remains the exact reconstruction path.
            db.execute_unprepared(
                r#"UPDATE messages
                  SET original_content_json = CASE
                      WHEN status = 'redacted' THEN '{"type":"redacted"}'
                      ELSE content_json
                  END"#,
            )
            .await?;
        }
        let revisions_columns_added =
            !crate::migration::column_exists(manager, "message_revisions", "redacted_at").await?;
        if revisions_columns_added {
            db.execute_unprepared("ALTER TABLE message_revisions ADD COLUMN redacted_at TIMESTAMP")
                .await?;
        }
        if !crate::migration::column_exists(manager, "message_revisions", "redacted_by").await? {
            db.execute_unprepared("ALTER TABLE message_revisions ADD COLUMN redacted_by TEXT")
                .await?;
        }
        db.execute_unprepared(
            r"CREATE INDEX IF NOT EXISTS idx_revisions_message_visible
              ON message_revisions(message_event_id, redacted_at)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_revisions_message_visible")
            .await?;
        db.execute_unprepared("ALTER TABLE message_revisions DROP COLUMN redacted_by")
            .await?;
        db.execute_unprepared("ALTER TABLE message_revisions DROP COLUMN redacted_at")
            .await?;
        db.execute_unprepared("ALTER TABLE messages DROP COLUMN matrix_event_type")
            .await?;
        db.execute_unprepared("ALTER TABLE messages DROP COLUMN original_content_json")
            .await?;
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "poll_responses";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Track the Matrix event ID of each poll vote so redactions can remove a
/// vote from the aggregate, plus the redaction metadata itself.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for (column, definition) in [
            ("event_id", "TEXT"),
            ("redacted_at", "DATETIME"),
            ("redacted_by", "TEXT"),
        ] {
            if !column_exists(manager, TABLE, column).await? {
                db.execute_unprepared(&format!(
                    "ALTER TABLE {TABLE} ADD COLUMN {column} {definition}"
                ))
                .await?;
            }
        }
        db.execute_unprepared(&format!(
            "CREATE INDEX IF NOT EXISTS idx_poll_responses_event \
             ON {TABLE}(event_id)"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_poll_responses_event")
            .await?;
        for column in ["event_id", "redacted_at", "redacted_by"] {
            if column_exists(manager, TABLE, column).await? {
                db.execute_unprepared(&format!("ALTER TABLE {TABLE} DROP COLUMN {column}"))
                    .await?;
            }
        }
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add secondary indexes for the hot intent-queue queries (status + due
/// time, and event-id completion lookups).
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_post_intent_status_attempt \
                 ON intent_queue_post_comment(status, next_attempt_at)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_delete_intent_status_attempt \
                 ON intent_queue_delete_comment(status, next_attempt_at)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_update_intent_status_attempt \
                 ON intent_queue_update_comment(status, next_attempt_at)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_post_intent_event_id \
                 ON intent_queue_post_comment(matrix_event_id)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_delete_intent_target \
                 ON intent_queue_delete_comment(target_event_id)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_update_intent_event_id \
                 ON intent_queue_update_comment(event_id)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for index in [
            "idx_post_intent_status_attempt",
            "idx_delete_intent_status_attempt",
            "idx_update_intent_status_attempt",
            "idx_post_intent_event_id",
            "idx_delete_intent_target",
            "idx_update_intent_event_id",
        ] {
            manager
                .get_connection()
                .execute_unprepared(&format!("DROP INDEX IF EXISTS {index}"))
                .await?;
        }
        Ok(())
    }
}

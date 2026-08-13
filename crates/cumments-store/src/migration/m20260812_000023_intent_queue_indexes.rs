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
                "CREATE INDEX IF NOT EXISTS idx_post_submission_status_attempt \
                 ON post_submissions(status, next_attempt_at)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_delete_submission_status_attempt \
                 ON delete_submissions(status, next_attempt_at)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_update_submission_status_attempt \
                 ON update_submissions(status, next_attempt_at)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_post_submission_event_id \
                 ON post_submissions(matrix_event_id)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_delete_submission_target \
                 ON delete_submissions(target_event_id)",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_update_submission_event_id \
                 ON update_submissions(event_id)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for index in [
            "idx_post_submission_status_attempt",
            "idx_delete_submission_status_attempt",
            "idx_update_submission_status_attempt",
            "idx_post_submission_event_id",
            "idx_delete_submission_target",
            "idx_update_submission_event_id",
        ] {
            manager
                .get_connection()
                .execute_unprepared(&format!("DROP INDEX IF EXISTS {index}"))
                .await?;
        }
        Ok(())
    }
}

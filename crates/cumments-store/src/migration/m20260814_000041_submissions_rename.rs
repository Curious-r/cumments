use sea_orm_migration::prelude::*;

use crate::migration::{column_exists, table_exists};

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Rename the write queue to "submissions" vocabulary: the three
/// `intent_queue_*` tables become `*_submissions`, and the public
/// correlation columns `intent_id` become `submission_id`.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for (old, new) in [
            ("intent_queue_post_comment", "post_submissions"),
            ("intent_queue_update_comment", "update_submissions"),
            ("intent_queue_delete_comment", "delete_submissions"),
        ] {
            if table_exists(manager, old).await? && !table_exists(manager, new).await? {
                db.execute_unprepared(&format!("ALTER TABLE {old} RENAME TO {new}"))
                    .await?;
            }
        }
        if column_exists(manager, "messages", "intent_id").await? {
            db.execute_unprepared("ALTER TABLE messages RENAME COLUMN intent_id TO submission_id")
                .await?;
        }
        if column_exists(manager, "idempotency_keys", "intent_id").await? {
            db.execute_unprepared(
                "ALTER TABLE idempotency_keys RENAME COLUMN intent_id TO submission_id",
            )
            .await?;
        }
        // Recreate the historical queue indexes under submission names.
        for (old, create) in [
            (
                "idx_post_intent_status_attempt",
                "CREATE INDEX IF NOT EXISTS idx_post_submission_status_attempt \
                 ON post_submissions(status, next_attempt_at)",
            ),
            (
                "idx_delete_intent_status_attempt",
                "CREATE INDEX IF NOT EXISTS idx_delete_submission_status_attempt \
                 ON delete_submissions(status, next_attempt_at)",
            ),
            (
                "idx_update_intent_status_attempt",
                "CREATE INDEX IF NOT EXISTS idx_update_submission_status_attempt \
                 ON update_submissions(status, next_attempt_at)",
            ),
            (
                "idx_post_intent_event_id",
                "CREATE INDEX IF NOT EXISTS idx_post_submission_event_id \
                 ON post_submissions(matrix_event_id)",
            ),
            (
                "idx_delete_intent_target",
                "CREATE INDEX IF NOT EXISTS idx_delete_submission_target \
                 ON delete_submissions(target_event_id)",
            ),
            (
                "idx_update_intent_event_id",
                "CREATE INDEX IF NOT EXISTS idx_update_submission_event_id \
                 ON update_submissions(event_id)",
            ),
        ] {
            db.execute_unprepared(&format!("DROP INDEX IF EXISTS {old}"))
                .await?;
            db.execute_unprepared(create).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for (new, old) in [
            ("post_submissions", "intent_queue_post_comment"),
            ("update_submissions", "intent_queue_update_comment"),
            ("delete_submissions", "intent_queue_delete_comment"),
        ] {
            if table_exists(manager, new).await? && !table_exists(manager, old).await? {
                db.execute_unprepared(&format!("ALTER TABLE {new} RENAME TO {old}"))
                    .await?;
            }
        }
        if column_exists(manager, "messages", "submission_id").await? {
            db.execute_unprepared("ALTER TABLE messages RENAME COLUMN submission_id TO intent_id")
                .await?;
        }
        if column_exists(manager, "idempotency_keys", "submission_id").await? {
            db.execute_unprepared(
                "ALTER TABLE idempotency_keys RENAME COLUMN submission_id TO intent_id",
            )
            .await?;
        }
        for index in [
            "idx_post_submission_status_attempt",
            "idx_delete_submission_status_attempt",
            "idx_update_submission_status_attempt",
            "idx_post_submission_event_id",
            "idx_delete_submission_target",
            "idx_update_submission_event_id",
        ] {
            db.execute_unprepared(&format!("DROP INDEX IF EXISTS {index}"))
                .await?;
        }
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Unifies transaction-ID handling across all submission queues:
/// - delete/update rows persist their chosen transaction ID like posts;
/// - delete/update rows record the Matrix event ID returned by the send so the
///   timeout pass can verify it and clear the transaction ID on ghosts;
/// - the now-redundant `force_new_txn` flag is dropped from posts.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for table in ["delete_submissions", "update_submissions"] {
            if !column_exists(manager, table, "txn_id").await? {
                db.execute_unprepared(&format!("ALTER TABLE {table} ADD COLUMN txn_id TEXT"))
                    .await?;
            }
            if !column_exists(manager, table, "matrix_event_id").await? {
                db.execute_unprepared(&format!(
                    "ALTER TABLE {table} ADD COLUMN matrix_event_id TEXT"
                ))
                .await?;
            }
        }
        if column_exists(manager, "post_submissions", "force_new_txn").await? {
            db.execute_unprepared("ALTER TABLE post_submissions DROP COLUMN force_new_txn")
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if !column_exists(manager, "post_submissions", "force_new_txn").await? {
            db.execute_unprepared(
                "ALTER TABLE post_submissions \
                 ADD COLUMN force_new_txn BOOLEAN NOT NULL DEFAULT 0",
            )
            .await?;
        }
        for table in ["delete_submissions", "update_submissions"] {
            if column_exists(manager, table, "matrix_event_id").await? {
                db.execute_unprepared(&format!("ALTER TABLE {table} DROP COLUMN matrix_event_id"))
                    .await?;
            }
            if column_exists(manager, table, "txn_id").await? {
                db.execute_unprepared(&format!("ALTER TABLE {table} DROP COLUMN txn_id"))
                    .await?;
            }
        }
        Ok(())
    }
}

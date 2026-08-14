use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Marks a post submission whose previous send was confirmed absent on the
/// homeserver. The reconciler then uses a fresh transaction ID on the next
/// attempt instead of reusing the deterministic ID that now points at a
/// ghost event.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if !column_exists(manager, "post_submissions", "force_new_txn").await? {
            db.execute_unprepared(
                "ALTER TABLE post_submissions \
                 ADD COLUMN force_new_txn BOOLEAN NOT NULL DEFAULT 0",
            )
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if column_exists(manager, "post_submissions", "force_new_txn").await? {
            db.execute_unprepared("ALTER TABLE post_submissions DROP COLUMN force_new_txn")
                .await?;
        }
        Ok(())
    }
}

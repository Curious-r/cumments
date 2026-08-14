use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Persists the transaction ID chosen for each post submission's send
/// attempt. Random IDs avoid homeserver transaction maps that return ghost
/// event IDs for deterministic `cumments_post_<id>` names, and storing them
/// keeps retries idempotent.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if !column_exists(manager, "post_submissions", "txn_id").await? {
            db.execute_unprepared("ALTER TABLE post_submissions ADD COLUMN txn_id TEXT")
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if column_exists(manager, "post_submissions", "txn_id").await? {
            db.execute_unprepared("ALTER TABLE post_submissions DROP COLUMN txn_id")
                .await?;
        }
        Ok(())
    }
}

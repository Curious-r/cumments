use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add a processing-lease column to the submission queues so a crashed
/// reconciler's in-flight rows can be recovered instead of hanging forever.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for table in [
            "post_submissions",
            "update_submissions",
            "delete_submissions",
        ] {
            if !column_exists(manager, table, "lease_expires_at").await? {
                db.execute_unprepared(&format!(
                    "ALTER TABLE {table} ADD COLUMN lease_expires_at DATETIME"
                ))
                .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for table in [
            "post_submissions",
            "update_submissions",
            "delete_submissions",
        ] {
            if column_exists(manager, table, "lease_expires_at").await? {
                db.execute_unprepared(&format!("ALTER TABLE {table} DROP COLUMN lease_expires_at"))
                    .await?;
            }
        }
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "sites";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add the decommission lifecycle column. Existing rows are `active`, so the
/// default keeps their behavior unchanged.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if !column_exists(manager, TABLE, "lifecycle_status").await? {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} ADD COLUMN lifecycle_status TEXT NOT NULL DEFAULT 'active'"
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if column_exists(manager, TABLE, "lifecycle_status").await? {
            db.execute_unprepared(&format!("ALTER TABLE {TABLE} DROP COLUMN lifecycle_status"))
                .await?;
        }
        Ok(())
    }
}

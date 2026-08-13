use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "sites";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Mark sites registered under a caller-chosen id. Chosen ids are a
/// privilege: in `optional` mode they must verify an origin before writes
/// are accepted. Existing rows are backfilled by shape — ids that are not 32
/// lowercase hex characters are treated as caller-chosen.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if !column_exists(manager, TABLE, "is_custom_id").await? {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} ADD COLUMN is_custom_id INTEGER NOT NULL DEFAULT 0"
            ))
            .await?;
            db.execute_unprepared(&format!(
                "UPDATE {TABLE} SET is_custom_id = CASE \
                 WHEN length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*' THEN 0 ELSE 1 END"
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if column_exists(manager, TABLE, "is_custom_id").await? {
            db.execute_unprepared(&format!("ALTER TABLE {TABLE} DROP COLUMN is_custom_id"))
                .await?;
        }
        Ok(())
    }
}

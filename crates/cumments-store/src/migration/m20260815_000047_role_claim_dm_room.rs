use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Records the DM room the AppService bot joined for a token-DM role claim,
/// so the bot can leave once the claim reaches a terminal state.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if !column_exists(manager, "role_claims", "dm_room_id").await? {
            db.execute_unprepared("ALTER TABLE role_claims ADD COLUMN dm_room_id TEXT")
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if column_exists(manager, "role_claims", "dm_room_id").await? {
            db.execute_unprepared("ALTER TABLE role_claims DROP COLUMN dm_room_id")
                .await?;
        }
        Ok(())
    }
}

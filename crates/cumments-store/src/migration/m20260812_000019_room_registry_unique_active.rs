use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Enforce a single active room per `(site_id, post_slug)` so room lookups
/// are deterministic and edits/redactions cannot target a duplicate room.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Deactivate duplicates deterministically: keep the oldest row per
        // (site_id, post_slug), matching the current `get_registered_room`
        // convention of preferring the first registered room.
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE room_registry SET is_active = 0, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE is_active = 1 AND rowid NOT IN ( \
                     SELECT MIN(rowid) FROM room_registry \
                     WHERE is_active = 1 GROUP BY site_id, post_slug \
                 )",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_room_registry_active_site_post \
                 ON room_registry(site_id, post_slug) WHERE is_active = 1",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_room_registry_active_site_post")
            .await?;
        Ok(())
    }
}

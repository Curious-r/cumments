use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Enforces uniqueness on (site_id, author_public_key, field, sequence)
/// in profile_operations to guarantee race-free serialization streams.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // If duplicate sequences exist from concurrent allocations prior to this migration,
        // prune older duplicates deterministically so the unique index can be created cleanly.
        db.execute_unprepared(
            "DELETE FROM profile_operations AS p \
             WHERE EXISTS ( \
                 SELECT 1 FROM profile_operations AS newer \
                 WHERE newer.site_id = p.site_id \
                   AND newer.author_public_key = p.author_public_key \
                   AND newer.field = p.field \
                   AND newer.sequence = p.sequence \
                   AND (newer.created_at > p.created_at \
                        OR (newer.created_at = p.created_at AND newer.id > p.id)) \
             );",
        )
        .await?;

        db.execute_unprepared(
            "DROP INDEX IF EXISTS idx_profile_ops_site_author_field; \
             CREATE UNIQUE INDEX IF NOT EXISTS idx_profile_ops_site_author_field \
             ON profile_operations (site_id, author_public_key, field, sequence);",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS idx_profile_ops_site_author_field; \
                 CREATE INDEX IF NOT EXISTS idx_profile_ops_site_author_field \
                 ON profile_operations (site_id, author_public_key, field, sequence);",
            )
            .await?;
        Ok(())
    }
}

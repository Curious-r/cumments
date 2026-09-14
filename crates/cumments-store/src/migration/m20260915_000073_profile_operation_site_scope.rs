use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Replaces (author_public_key, field, sequence) index on profile_operations
/// with a site-scoped (site_id, author_public_key, field, sequence) index.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS idx_profile_ops_author_field; \
                 CREATE INDEX IF NOT EXISTS idx_profile_ops_site_author_field ON profile_operations (site_id, author_public_key, field, sequence);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS idx_profile_ops_site_author_field; \
                 CREATE INDEX IF NOT EXISTS idx_profile_ops_author_field ON profile_operations (author_public_key, field, sequence);",
            )
            .await?;
        Ok(())
    }
}

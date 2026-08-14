use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Binds media uploads to the post submission that references them, so the
/// orphan sweep never deletes media still needed by a retrying submission.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if !column_exists(manager, "media_uploads", "submission_id").await? {
            db.execute_unprepared("ALTER TABLE media_uploads ADD COLUMN submission_id INTEGER")
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if column_exists(manager, "media_uploads", "submission_id").await? {
            db.execute_unprepared("ALTER TABLE media_uploads DROP COLUMN submission_id")
                .await?;
        }
        Ok(())
    }
}

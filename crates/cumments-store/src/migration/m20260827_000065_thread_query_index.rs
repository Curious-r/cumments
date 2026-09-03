use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Thread query: WHERE site_id = ? AND page_slug = ? AND thread_root = ?
        // AND status = 'active' ORDER BY timestamp DESC, event_id ASC
        // Existing idx_messages_site_post covers (site_id, page_slug) but cannot
        // efficiently seek on thread_root. Create a minimal composite that at
        // least covers (site_id, page_slug, thread_root) and appends the sort
        // columns so the ORDER BY can be satisfied without a separate sort
        // (SQLite can traverse an ASC index backwards for DESC, but we spell
        // the intended direction explicitly). Status is an equality filter on a
        // low-cardinality column and is left to the residual filter; the seek
        // on (site_id, page_slug, thread_root) already narrows to the thread
        // replies, which are typically small.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages (site_id, page_slug, thread_root, timestamp DESC, event_id ASC)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_messages_thread")
            .await?;
        Ok(())
    }
}

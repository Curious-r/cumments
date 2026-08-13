use sea_orm_migration::prelude::*;

const TABLE: &str = "poll_responses";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Enforce one live vote row per `(poll_message_id, sender_mxid)`.
///
/// Before migration, concurrent projections could race the read-then-insert
/// in `save_poll_vote` and leave duplicate rows for the same voter, which
/// double-counted in the poll aggregate. Existing duplicates are collapsed to
/// the newest vote (highest `origin_server_ts`, then highest row id) before
/// the unique index is created.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(&format!(
            "DELETE FROM {TABLE} AS p \
             WHERE EXISTS ( \
                 SELECT 1 FROM {TABLE} AS newer \
                 WHERE newer.poll_message_id = p.poll_message_id \
                   AND newer.sender_mxid = p.sender_mxid \
                   AND (newer.origin_server_ts > p.origin_server_ts \
                        OR (newer.origin_server_ts = p.origin_server_ts \
                            AND newer.id > p.id)) \
             )"
        ))
        .await?;
        db.execute_unprepared(&format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_poll_responses_sender \
             ON {TABLE}(poll_message_id, sender_mxid)"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS uq_poll_responses_sender")
            .await?;
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add `origin_server_ts` and `event_id` columns to `room_members` projection table.
///
/// These columns record the deterministic projection ordering version for monotonic
/// application ordering, preventing late-arriving or out-of-order member events from
/// regressing the current room presentation projection.
///
/// Because `room_members` is a disposable projection rebuildable from canonical Matrix
/// state events, existing projection rows are discarded during migration so that the
/// projection can be cleanly repopulated with authoritative event identity and timestamps.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Discard existing projection data. It will be repopulated from canonical Matrix state events.
        db.execute_unprepared("DELETE FROM room_members;").await?;

        if !column_exists(manager, "room_members", "origin_server_ts").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .add_column(
                            ColumnDef::new(Alias::new("origin_server_ts"))
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(manager, "room_members", "event_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .add_column(ColumnDef::new(Alias::new("event_id")).string().null())
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, "room_members", "event_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .drop_column(Alias::new("event_id"))
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(manager, "room_members", "origin_server_ts").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .drop_column(Alias::new("origin_server_ts"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

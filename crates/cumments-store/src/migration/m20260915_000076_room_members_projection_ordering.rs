use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add `origin_server_ts` and nullable `event_id` columns to `room_members` projection table.
///
/// These columns record the deterministic projection ordering version for monotonic
/// application ordering, preventing late-arriving or out-of-order member events from
/// regressing the current room presentation projection.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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

            manager
                .get_connection()
                .execute_unprepared(
                    "UPDATE room_members \
                     SET origin_server_ts = CAST((strftime('%s', updated_at) * 1000) AS INTEGER) \
                     WHERE origin_server_ts = 0 AND updated_at IS NOT NULL;",
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

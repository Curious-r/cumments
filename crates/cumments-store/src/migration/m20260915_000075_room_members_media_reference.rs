use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add nullable `media_reference` column to `room_members` projection table.
///
/// For current room presentation, member-event processing associates
/// avatar MXCs with durable `MediaReference` identities.
/// The `room_members` table is a materialized projection and survives rebuilds
/// by resolving against durable `media_references`.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, "room_members", "media_reference").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .add_column(
                            ColumnDef::new(Alias::new("media_reference"))
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, "room_members", "media_reference").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .drop_column(Alias::new("media_reference"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

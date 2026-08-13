use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Record the Matrix room a sent intent targets, so a timed-out
/// `waiting_for_sync` intent can verify its event on the homeserver
/// before deciding whether a resend is safe.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "post_submissions",
            "delete_submissions",
            "update_submissions",
        ] {
            if !column_exists(manager, table, "room_id").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(ColumnDef::new(Alias::new("room_id")).string().null())
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "post_submissions",
            "delete_submissions",
            "update_submissions",
        ] {
            if column_exists(manager, table, "room_id").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .drop_column(Alias::new("room_id"))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const COMMENTS_TABLE: &str = "comments";
const LAST_EDIT_TS: &str = "last_edit_ts";
const LAST_EDIT_EVENT_ID: &str = "last_edit_event_id";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Track the last applied edit on the read model so stale or out-of-order
/// edits cannot regress newer content.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, COMMENTS_TABLE, LAST_EDIT_TS).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(COMMENTS_TABLE))
                        .add_column(
                            ColumnDef::new(Alias::new(LAST_EDIT_TS))
                                .big_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !column_exists(manager, COMMENTS_TABLE, LAST_EDIT_EVENT_ID).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(COMMENTS_TABLE))
                        .add_column(
                            ColumnDef::new(Alias::new(LAST_EDIT_EVENT_ID))
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
        for column in [LAST_EDIT_TS, LAST_EDIT_EVENT_ID] {
            if column_exists(manager, COMMENTS_TABLE, column).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(COMMENTS_TABLE))
                            .drop_column(Alias::new(column))
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

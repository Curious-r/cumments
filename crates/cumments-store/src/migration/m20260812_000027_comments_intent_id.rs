use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "comments";
const COLUMN: &str = "submission_id";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Records which intent produced a projected comment so API clients can
/// correlate their `202 { submission_id }` response with the read model and SSE.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .add_column(ColumnDef::new(Alias::new(COLUMN)).big_integer().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .drop_column(Alias::new(COLUMN))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

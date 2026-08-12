use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "room_registry";
const COLUMN: &str = "blocked_reason";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Record why a room could not be adopted, so operators can see blocked
/// rooms instead of only inferring them from retry logs.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .add_column(ColumnDef::new(Alias::new(COLUMN)).string().null())
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

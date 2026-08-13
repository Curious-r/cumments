use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "room_registry";
const COLUMN: &str = "blocked_reason";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Drop the orphaned legacy `blocked_reason` column left on fresh databases.
///
/// Migration 000001 builds `room_registry` from the current entity, which
/// already carries `quarantine_reason`; migration 000025 then adds the legacy
/// `blocked_reason` unconditionally, and 000030 only renames it when
/// `quarantine_reason` is absent. Fresh schemas therefore end up with both
/// columns. This migration drops the orphan so fresh and upgraded schemas
/// converge on the same shape.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
}

use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "post_submissions";
const COLUMN: &str = "timeout_confirmations";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Track consecutive timeout-pass confirmations that an event exists on the
/// homeserver, so dead-lettering requires a grace period instead of firing
/// on the first delayed projection.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .add_column(
                            ColumnDef::new(Alias::new(COLUMN))
                                .integer()
                                .not_null()
                                .default(0),
                        )
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

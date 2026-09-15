use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Drop `display_name` column from `post_submissions` table.
///
/// Severing legacy coupling between content posting and visitor Matrix display name mutations.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, "post_submissions", "display_name").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("post_submissions"))
                        .drop_column(Alias::new("display_name"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, "post_submissions", "display_name").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("post_submissions"))
                        .add_column(ColumnDef::new(Alias::new("display_name")).string().null())
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Add nullable `author_media_reference` column to `messages` table.
///
/// Materializes the durable MediaReference identity for the author's historical
/// avatar snapshot at event DAG context.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, "messages", "author_media_reference").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("messages"))
                        .add_column(
                            ColumnDef::new(Alias::new("author_media_reference"))
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
        if column_exists(manager, "messages", "author_media_reference").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("messages"))
                        .drop_column(Alias::new("author_media_reference"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

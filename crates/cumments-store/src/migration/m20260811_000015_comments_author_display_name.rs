use sea_orm_migration::prelude::*;

/// Rename `comments.author_displayname` to `author_display_name` so the
/// database column follows idiomatic snake_case word separation; the Matrix
/// wire key stays `displayname`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "comments", "author_displayname").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .rename_column(
                            Alias::new("author_displayname"),
                            Alias::new("author_display_name"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "comments", "author_display_name").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .rename_column(
                            Alias::new("author_display_name"),
                            Alias::new("author_displayname"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

/// Rename `comments.author_nickname` to `author_displayname` to match the
/// Matrix profile field name and the unified Cumments terminology.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "comments", "author_nickname").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .rename_column(
                            Alias::new("author_nickname"),
                            Alias::new("author_displayname"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "comments", "author_displayname").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .rename_column(
                            Alias::new("author_displayname"),
                            Alias::new("author_nickname"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

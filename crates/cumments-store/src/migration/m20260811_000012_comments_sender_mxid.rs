use sea_orm_migration::prelude::*;

/// Rename `comments.author_mxid` to `sender_mxid`: the column stores the
/// Matrix sender of the event, including virtual users for guest comments.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "comments", "author_mxid").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .rename_column(Alias::new("author_mxid"), Alias::new("sender_mxid"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "comments", "sender_mxid").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .rename_column(Alias::new("sender_mxid"), Alias::new("author_mxid"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::entities::*;
use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, "comments", "author_token_hash").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(comments::Entity)
                        .add_column(
                            ColumnDef::new(Alias::new("author_token_hash"))
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(manager, "intent_queue_post_comment", "author_token_hash").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(intent_queue_post_comment::Entity)
                        .add_column(
                            ColumnDef::new(Alias::new("author_token_hash"))
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
        if column_exists(manager, "comments", "author_token_hash").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(comments::Entity)
                        .drop_column(Alias::new("author_token_hash"))
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(manager, "intent_queue_post_comment", "author_token_hash").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(intent_queue_post_comment::Entity)
                        .drop_column(Alias::new("author_token_hash"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(comments::Entity)
                    .add_column(
                        ColumnDef::new(comments::Column::AuthorTokenHash)
                            .string()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(intent_queue_post_comment::Entity)
                    .add_column(
                        ColumnDef::new(intent_queue_post_comment::Column::AuthorTokenHash)
                            .string()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(comments::Entity)
                    .drop_column(comments::Column::AuthorTokenHash)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(intent_queue_post_comment::Entity)
                    .drop_column(intent_queue_post_comment::Column::AuthorTokenHash)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

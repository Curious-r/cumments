use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Replace the text-only `comments` read model with the rich `messages`
/// model: messages (typed content + raw escape hatch), edit revisions,
/// reactions and poll responses. The legacy `comments` table is dropped.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(messages::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(message_revisions::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(reactions::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(poll_responses::Entity))
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_messages_site_post")
                    .table(messages::Entity)
                    .col(messages::Column::SiteId)
                    .col(messages::Column::PostSlug)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_reactions_message")
                    .table(reactions::Entity)
                    .col(reactions::Column::MessageEventId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_revisions_message")
                    .table(message_revisions::Entity)
                    .col(message_revisions::Column::MessageEventId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ux_poll_responses_voter")
                    .table(poll_responses::Entity)
                    .col(poll_responses::Column::PollMessageId)
                    .col(poll_responses::Column::SenderMxid)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // The legacy read-model table is superseded by `messages`.
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS comments")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .drop_table(Table::drop().table(poll_responses::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(reactions::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(message_revisions::Entity).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(messages::Entity).to_owned())
            .await?;
        manager
            .create_table(schema.create_table_from_entity(comments::Entity))
            .await?;
        Ok(())
    }
}

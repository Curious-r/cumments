use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Process-state table backing token-DM verification of governance roles.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(role_claims::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("uq_role_claim_scope")
                    .table(role_claims::Entity)
                    .col(role_claims::Column::SiteId)
                    .col(role_claims::Column::RoomId)
                    .col(role_claims::Column::UserId)
                    .col(role_claims::Column::Level)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_role_claim_user_status")
                    .table(role_claims::Entity)
                    .col(role_claims::Column::UserId)
                    .col(role_claims::Column::Status)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(role_claims::Entity).to_owned())
            .await?;
        Ok(())
    }
}

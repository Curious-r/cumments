use sea_orm_migration::prelude::*;

use crate::entities::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Durable profile mutation state machine and strict same-field serialization.
///
/// Tracks client operation identity, visitor public key, targeted profile field,
/// target value, monotonic sequence, and 6-state execution lifecycle.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);

        manager
            .create_table(schema.create_table_from_entity(profile_operations::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_profile_ops_author_field")
                    .table(profile_operations::Entity)
                    .col(profile_operations::Column::AuthorPublicKey)
                    .col(profile_operations::Column::Field)
                    .col(profile_operations::Column::Sequence)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_profile_ops_status")
                    .table(profile_operations::Entity)
                    .col(profile_operations::Column::Status)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(profile_operations::Entity).to_owned())
            .await?;
        Ok(())
    }
}

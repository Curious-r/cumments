use sea_orm_migration::prelude::*;

use crate::entities::command_audit_logs;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Audit log for chat-driven management commands.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(schema.create_table_from_entity(command_audit_logs::Entity))
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_command_audit_actor_created")
                    .table(command_audit_logs::Entity)
                    .col(command_audit_logs::Column::ActorMxid)
                    .col(command_audit_logs::Column::CreatedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(command_audit_logs::Entity).to_owned())
            .await?;
        Ok(())
    }
}

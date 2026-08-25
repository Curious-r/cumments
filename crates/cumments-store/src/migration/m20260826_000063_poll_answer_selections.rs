use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Preserves normalized MSC3381 selections without retaining arbitrary raw
/// payloads. Existing single-choice rows remain readable through their legacy
/// `option_index`.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Fresh databases create this table from the latest entity and already
        // contain both columns; upgrades need explicit additive migrations.
        if !column_exists(manager, "poll_response_events", "answer_ids_json").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(poll_response_events_alias())
                        .add_column(
                            ColumnDef::new(Alias::new("answer_ids_json"))
                                .string()
                                .not_null()
                                .default("[]"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !column_exists(manager, "poll_response_events", "spoiled_reason").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(poll_response_events_alias())
                        .add_column(ColumnDef::new(Alias::new("spoiled_reason")).string().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(poll_response_events_alias())
                    .drop_column(Alias::new("spoiled_reason"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(poll_response_events_alias())
                    .drop_column(Alias::new("answer_ids_json"))
                    .to_owned(),
            )
            .await
    }
}

fn poll_response_events_alias() -> Alias {
    Alias::new("poll_response_events")
}

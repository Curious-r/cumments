use sea_orm::entity::prelude::*;

/// Durable record of an acknowledged AppService transaction. Event-level
/// idempotency remains the correctness backstop if the process crashes after
/// projection but before this row is written.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "processed_appservice_transactions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub txn_id: String,
    pub processed_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

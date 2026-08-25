use sea_orm::entity::prelude::*;

/// Durable projector-event publications. A row is reserved before projection,
/// filled after the projection transaction commits, and marked sent only after
/// broadcast. This turns process crashes into bounded at-least-once delivery.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sse_outbox")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub delivery_key: String,
    /// `None` while its Matrix event is being processed.
    pub payload_json: Option<String>,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

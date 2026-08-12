use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "reactions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The reaction event ID (m.reaction).
    #[sea_orm(unique)]
    pub event_id: String,
    pub message_event_id: String,
    pub sender_mxid: String,
    pub key: String,
    pub origin_server_ts: i64,
    pub redacted_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

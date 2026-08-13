use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "room_state_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub event_id: String,
    pub room_id: String,
    pub event_type: String,
    pub state_key: String,
    pub sender: String,
    pub origin_server_ts: i64,
    /// Raw event content (system-message payload).
    pub content_json: String,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

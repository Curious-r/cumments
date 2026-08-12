use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "poll_responses")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub poll_message_id: String,
    pub sender_mxid: String,
    pub option_index: i64,
    pub origin_server_ts: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

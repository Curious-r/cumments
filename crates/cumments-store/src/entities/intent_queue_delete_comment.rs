use sea_orm::entity::prelude::*;

use super::active_enums::IntentStatus;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "intent_queue_delete_comment")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(column_type = "Text")]
    pub payload: String,
    pub status: IntentStatus,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub target_event_id: Option<String>,
}

impl ActiveModelBehavior for ActiveModel {}

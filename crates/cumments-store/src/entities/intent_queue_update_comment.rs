use sea_orm::entity::prelude::*;

use super::active_enums::IntentStatus;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "intent_queue_update_comment")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub site_id: String,
    pub post_slug: String,
    pub event_id: String,
    pub room_id: Option<String>,
    pub content: String,
    pub author_public_key: Option<String>,
    pub author_signature: Option<String>,
    pub author_challenge: Option<String>,
    pub status: IntentStatus,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub retry_count: i64,
    pub next_attempt_at: Option<DateTimeUtc>,
    pub last_error: Option<String>,
}

impl ActiveModelBehavior for ActiveModel {}

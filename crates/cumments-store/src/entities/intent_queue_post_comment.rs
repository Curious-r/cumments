use sea_orm::entity::prelude::*;

use super::active_enums::IntentStatus;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "intent_queue_post_comment")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(column_type = "Text")]
    pub payload: String,
    pub status: IntentStatus,
    pub retry_count: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub matrix_event_id: Option<String>,
    pub room_id: Option<String>,
    pub author_public_key: Option<String>,
    pub next_attempt_at: Option<DateTimeUtc>,
    pub last_error: Option<String>,
    /// How many consecutive timeout passes observed the event as existing
    /// on the homeserver. Dead-lettering requires several confirmations so a
    /// delayed projection is not treated as a failure.
    pub timeout_confirmations: i64,
}

impl ActiveModelBehavior for ActiveModel {}

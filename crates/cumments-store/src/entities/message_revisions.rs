use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "message_revisions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The edit event ID (m.replace).
    #[sea_orm(unique)]
    pub event_id: String,
    /// The edited message's event ID.
    pub message_event_id: String,
    /// Serialized `Content` after the edit.
    pub content_json: String,
    pub edited_at: DateTimeUtc,
    pub editor_mxid: String,
    pub redacted_at: Option<DateTimeUtc>,
    pub redacted_by: Option<String>,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

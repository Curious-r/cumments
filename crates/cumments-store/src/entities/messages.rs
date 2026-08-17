use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "messages")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub event_id: String,
    pub room_id: String,
    pub site_id: String,
    pub page_slug: String,
    pub sender_mxid: String,
    /// One of `visitor` / `matrix` (see `AuthorKind`).
    pub author_kind: String,
    pub author_display_name: Option<String>,
    pub author_avatar_url: Option<String>,
    pub author_public_key: Option<String>,
    /// Serialized `Content` (the read-model payload).
    pub content_json: String,
    /// Raw Matrix event content (forward-compatibility escape hatch).
    pub raw_content_json: String,
    pub timestamp: DateTimeUtc,
    pub reply_to: Option<String>,
    pub thread_root: Option<String>,
    /// One of `active` / `redacted` (see `MessageStatus`).
    pub status: String,
    pub redacted_at: Option<DateTimeUtc>,
    pub redacted_by: Option<String>,
    pub submission_id: Option<i64>,
    /// `origin_server_ts` of the last applied edit, used for recency checks.
    pub last_edit_ts: Option<i64>,
    /// Event ID of the last applied edit (deterministic tie-breaker).
    pub last_edit_event_id: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

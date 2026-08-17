//! Legacy comments entity, kept only so historical migrations (000001-000029)
//! can create/alter the old table on fresh databases. The read model uses
//! `messages` (see `migration 000031`), which drops the `comments` table.
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "comments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub event_id: String,
    pub room_id: String,
    pub site_id: String,
    pub page_slug: String,
    pub sender_mxid: String,
    pub author_type: String,
    pub author_display_name: Option<String>,
    pub content: String,
    pub timestamp: DateTimeUtc,
    pub reply_to: Option<String>,
    /// Queue row ID of the submission that produced this comment, when submitted
    /// through the Cumments API; `None` for Matrix-native comments.
    pub submission_id: Option<i64>,
    pub created_at: DateTimeUtc,
    pub author_public_key: Option<String>,
    /// Local read-model write time (when this row was last projected/updated
    /// locally). NOT a Matrix fact and NOT the comment's edit time.
    pub projected_at: DateTimeUtc,
    /// `origin_server_ts` of the last applied edit; `None` when the comment
    /// has never been edited.
    pub last_edit_ts: Option<i64>,
    /// Event ID of the last applied edit, used as a tie-breaker when two
    /// edits share a timestamp.
    pub last_edit_event_id: Option<String>,
}

impl ActiveModelBehavior for ActiveModel {}

use sea_orm::entity::prelude::*;

use super::active_enums::SubmissionStatus;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "update_submissions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub site_id: String,
    pub page_slug: String,
    pub event_id: String,
    pub room_id: Option<String>,
    /// The replacement event ID recorded when the submission moved to
    /// `waiting_for_sync`, used to verify the event actually exists.
    pub matrix_event_id: Option<String>,
    /// The transaction ID chosen for the latest send attempt, if any.
    pub txn_id: Option<String>,
    pub content: String,
    pub author_public_key: Option<String>,
    pub author_signature: Option<String>,
    pub author_challenge: Option<String>,
    pub status: SubmissionStatus,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub retry_count: i64,
    pub next_attempt_at: Option<DateTimeUtc>,
    /// When the processing lease expires; `NULL` unless claimed.
    pub lease_expires_at: Option<DateTimeUtc>,
    pub last_error: Option<String>,
}

impl ActiveModelBehavior for ActiveModel {}

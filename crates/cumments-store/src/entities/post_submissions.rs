use sea_orm::entity::prelude::*;

use super::active_enums::SubmissionStatus;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "post_submissions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(column_type = "Text")]
    pub payload: String,
    pub status: SubmissionStatus,
    pub retry_count: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub matrix_event_id: Option<String>,
    pub room_id: Option<String>,
    pub author_public_key: Option<String>,
    pub next_attempt_at: Option<DateTimeUtc>,
    /// When the processing lease expires; `NULL` unless claimed.
    pub lease_expires_at: Option<DateTimeUtc>,
    pub last_error: Option<String>,
    /// How many consecutive timeout passes observed the event as existing
    /// on the homeserver. Dead-lettering requires several confirmations so a
    /// delayed projection is not treated as a failure.
    pub timeout_confirmations: i64,
    /// Unix milliseconds of the last timeout confirmation, used to enforce a
    /// cooldown between confirmation passes.
    pub last_timeout_confirmation_at: Option<i64>,
    /// Consecutive timeout passes that failed to check event existence
    /// (network/homeserver errors). Dead-lettered after a threshold so
    /// submissions cannot sit in limbo forever.
    pub timeout_check_errors: i64,
    /// The transaction ID chosen for the latest send attempt, if any. `NULL`
    /// means the next attempt allocates a fresh one; a stored value is reused
    /// on retries so homeserver-side transaction idempotency is preserved.
    pub txn_id: Option<String>,
}

impl ActiveModelBehavior for ActiveModel {}

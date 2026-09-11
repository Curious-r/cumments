use sea_orm::entity::prelude::*;

/// One immutable Matrix `org.matrix.msc3381.poll.end` relation event.
///
/// The effective end is the earliest authorized, non-redacted end by
/// `(origin_server_ts, event_id)`; later ends are retained as facts but do not
/// replace it. `authorized` snapshots the creator/redact-power decision made
/// when the event was projected.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "poll_end_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub event_id: String,
    #[sea_orm(indexed)]
    pub poll_message_id: String,
    pub sender_mxid: String,
    pub authorized: bool,
    pub origin_server_ts: i64,
    pub redacted_at: Option<DateTimeUtc>,
    pub redacted_by: Option<String>,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

use sea_orm::entity::prelude::*;

/// One immutable Matrix `m.poll.response` relation event.
///
/// The current vote is derived by selecting each voter's latest non-redacted
/// event; this preserves the prior choice when a newer vote is redacted.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "poll_response_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub event_id: String,
    #[sea_orm(indexed)]
    pub poll_message_id: String,
    pub sender_mxid: String,
    /// `None` when the response is an unvote or its selections are invalid.
    pub option_index: Option<i64>,
    pub origin_server_ts: i64,
    pub redacted_at: Option<DateTimeUtc>,
    pub redacted_by: Option<String>,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

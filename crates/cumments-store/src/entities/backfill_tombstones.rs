use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "backfill_tombstones")]
pub struct Model {
    /// The redacted comment's Matrix event ID.
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: String,
    /// Room the redaction arrived from; used to avoid suppressing a comment
    /// with the same event ID in another room.
    pub room_id: String,
    /// The redaction event that deleted (or will delete) the target.
    pub redaction_event_id: String,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

use sea_orm::entity::prelude::*;

/// One bot-authorized native room upgrade. The row is written before the
/// homeserver call so a lost response cannot make a tombstone look unmanaged.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "room_upgrade_intents")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub old_room_id: String,
    pub expected_new_version: String,
    pub replacement_room_id: Option<String>,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

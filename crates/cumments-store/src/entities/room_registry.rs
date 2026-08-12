use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "room_registry")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub room_id: String,
    pub site_id: String,
    pub post_slug: String,
    pub is_active: bool,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// Why the room is blocked from adoption/use (e.g. governance check
    /// failed). `None` means the room is not blocked.
    pub blocked_reason: Option<String>,
}

impl ActiveModelBehavior for ActiveModel {}

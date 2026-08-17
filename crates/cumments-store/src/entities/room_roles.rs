use sea_orm::entity::prelude::*;

/// Projected room-level governance roles (owner 100 / global-moderator 75 /
/// moderator 50) from a comment room's `m.room.power_levels` state.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "room_roles")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub room_id: String,
    #[sea_orm(primary_key)]
    pub user_id: String,
    pub level: i64,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

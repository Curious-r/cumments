use sea_orm::entity::prelude::*;

/// Projected site-level governance roles (owner 100 / global-moderator 75) from
/// the site Space's `m.room.power_levels` state.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "site_roles")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub site_id: String,
    #[sea_orm(primary_key)]
    pub user_id: String,
    pub level: i64,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

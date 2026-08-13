use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "room_members")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub room_id: String,
    #[sea_orm(primary_key)]
    pub user_id: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    /// Matrix membership: `join`, `invite`, `leave`, `ban`.
    pub membership: String,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

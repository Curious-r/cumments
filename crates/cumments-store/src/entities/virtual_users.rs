use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "virtual_users")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub public_key: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub site_id: String,
    pub virtual_user_id: String,
    pub server_name: String,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "site_verified_origins")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub site_id: String,
    #[sea_orm(primary_key)]
    pub origin: String,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

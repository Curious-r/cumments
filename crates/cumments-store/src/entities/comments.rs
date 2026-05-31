use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "comments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub event_id: String,
    pub room_id: String,
    pub site_id: String,
    pub post_slug: String,
    pub author_mxid: String,
    pub author_nickname: Option<String>,
    pub content: String,
    pub timestamp: DateTimeUtc,
    pub created_at: DateTimeUtc,
    pub author_fingerprint: Option<String>,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

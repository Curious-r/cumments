use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "comments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub event_id: String,
    pub room_id: String,
    pub site_id: String,
    pub post_slug: String,
    pub author_mxid: String,
    pub author_type: String,
    pub author_nickname: Option<String>,
    pub content: String,
    pub timestamp: DateTimeUtc,
    pub reply_to: Option<String>,
    pub created_at: DateTimeUtc,
    pub author_public_key: Option<String>,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

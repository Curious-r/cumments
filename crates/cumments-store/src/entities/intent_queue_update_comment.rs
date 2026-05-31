use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "intent_queue_update_comment")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub site_id: String,
    pub post_slug: String,
    pub event_id: String,
    pub content: String,
    pub author_fingerprint: String,
    pub status: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

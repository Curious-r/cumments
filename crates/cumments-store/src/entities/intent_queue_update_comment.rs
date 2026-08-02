use sea_orm::entity::prelude::*;

use super::active_enums::IntentStatus;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "intent_queue_update_comment")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub site_id: String,
    pub post_slug: String,
    pub event_id: String,
    pub content: String,
    pub author_fingerprint: String,
    pub status: IntentStatus,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

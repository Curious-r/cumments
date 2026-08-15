use sea_orm::entity::prelude::*;

/// Projected site sticker packs (`m.room.image_pack` state on the site
/// Space). Disposable read-model cache; Matrix state is the source of truth.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sticker_packs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub site_id: String,
    #[sea_orm(primary_key)]
    pub state_key: String,
    pub room_id: String,
    pub event_id: String,
    pub sender: String,
    pub origin_server_ts: i64,
    /// Normalized, validated `m.room.image_pack` content.
    pub pack_json: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

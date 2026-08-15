use super::DbStore;
use crate::entities::sticker_packs;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::ports::StickerPackStore;
use cumments_core::sticker_packs::{StickerPack, StickerPackProjection};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

fn projection_from_model(model: sticker_packs::Model) -> Result<StickerPackProjection> {
    Ok(StickerPackProjection {
        pack: StickerPack {
            room_id: model.room_id,
            site_id: model.site_id,
            state_key: model.state_key,
            content: serde_json::from_str(&model.pack_json)?,
        },
        event_id: model.event_id,
        sender: model.sender,
        origin_server_ts: model.origin_server_ts,
    })
}

#[async_trait]
impl StickerPackStore for DbStore {
    async fn save_site_pack(&self, pack: &StickerPackProjection) -> Result<()> {
        let now = chrono::Utc::now();
        let model = sticker_packs::ActiveModel {
            site_id: Set(pack.pack.site_id.clone()),
            state_key: Set(pack.pack.state_key.clone()),
            room_id: Set(pack.pack.room_id.clone()),
            event_id: Set(pack.event_id.clone()),
            sender: Set(pack.sender.clone()),
            origin_server_ts: Set(pack.origin_server_ts),
            pack_json: Set(serde_json::to_string(&pack.pack.content)?),
            created_at: Set(now),
            updated_at: Set(now),
        };
        sticker_packs::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    sticker_packs::Column::SiteId,
                    sticker_packs::Column::StateKey,
                ])
                .update_columns([
                    sticker_packs::Column::RoomId,
                    sticker_packs::Column::EventId,
                    sticker_packs::Column::Sender,
                    sticker_packs::Column::OriginServerTs,
                    sticker_packs::Column::PackJson,
                    sticker_packs::Column::UpdatedAt,
                ])
                .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn list_site_packs(&self, site_id: &str) -> Result<Vec<StickerPackProjection>> {
        let models = sticker_packs::Entity::find()
            .filter(sticker_packs::Column::SiteId.eq(site_id))
            .order_by_asc(sticker_packs::Column::StateKey)
            .all(&self.db)
            .await?;
        models
            .into_iter()
            .map(projection_from_model)
            .collect::<Result<Vec<_>>>()
    }

    async fn get_site_pack(
        &self,
        site_id: &str,
        state_key: &str,
    ) -> Result<Option<StickerPackProjection>> {
        let model = sticker_packs::Entity::find_by_id((site_id.to_string(), state_key.to_string()))
            .one(&self.db)
            .await?;
        model.map(projection_from_model).transpose()
    }

    async fn delete_site_pack(&self, site_id: &str, state_key: &str) -> Result<()> {
        sticker_packs::Entity::delete_many()
            .filter(sticker_packs::Column::SiteId.eq(site_id))
            .filter(sticker_packs::Column::StateKey.eq(state_key))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn find_pack_by_event_id(&self, event_id: &str) -> Result<Option<(String, String)>> {
        let model = sticker_packs::Entity::find()
            .filter(sticker_packs::Column::EventId.eq(event_id))
            .one(&self.db)
            .await?;
        Ok(model.map(|m| (m.site_id, m.state_key)))
    }
}

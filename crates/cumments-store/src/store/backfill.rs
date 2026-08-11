use super::DbStore;
use crate::entities::backfill_cursors;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::ports::BackfillCursorStore;
use sea_orm::{EntityTrait, Set};

#[async_trait]
impl BackfillCursorStore for DbStore {
    async fn get_cursor(&self, room_id: &str) -> Result<Option<String>> {
        let model = backfill_cursors::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;
        Ok(model.and_then(|m| m.next_token))
    }

    async fn save_cursor(&self, room_id: &str, next_token: &str) -> Result<()> {
        let active_model = backfill_cursors::ActiveModel {
            room_id: Set(room_id.to_owned()),
            next_token: Set(Some(next_token.to_owned())),
            updated_at: Set(chrono::Utc::now()),
        };

        backfill_cursors::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(backfill_cursors::Column::RoomId)
                    .update_columns([
                        backfill_cursors::Column::NextToken,
                        backfill_cursors::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }
}

use super::DbStore;
use crate::entities::sse_outbox;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use cumments_core::models::SseOutbox;
use cumments_core::ports::SseOutboxStore;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

#[async_trait]
impl SseOutboxStore for DbStore {
    async fn reserve_sse_outbox(
        &self,
        txn_id: &str,
        event_index: u32,
        sse_event_id: &str,
    ) -> Result<bool> {
        let _ = (txn_id, event_index);
        let key = sse_event_id.to_owned();
        if let Some(existing) = sse_outbox::Entity::find()
            .filter(sse_outbox::Column::DeliveryKey.eq(&key))
            .one(&self.db)
            .await?
        {
            return Ok(existing.payload_json.is_none());
        }

        sse_outbox::ActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            delivery_key: Set(key),
            payload_json: Set(None),
            created_at: Set(Utc::now()),
        }
        .insert(&self.db)
        .await?;
        Ok(true)
    }

    async fn fill_sse_outbox(&self, sse_event_id: &str, payload_json: &str) -> Result<()> {
        let result = sse_outbox::Entity::update_many()
            .col_expr(
                sse_outbox::Column::PayloadJson,
                sea_orm::sea_query::Expr::value(Some(payload_json.to_owned())),
            )
            .filter(sse_outbox::Column::DeliveryKey.eq(sse_event_id))
            .filter(sse_outbox::Column::PayloadJson.is_null())
            .exec(&self.db)
            .await?;
        if result.rows_affected == 0 {
            anyhow::bail!("reserved SSE outbox row {sse_event_id} not found");
        }
        Ok(())
    }

    async fn pending_sse_outbox(&self, limit: u64) -> Result<Vec<SseOutbox>> {
        let rows = sse_outbox::Entity::find()
            .filter(sse_outbox::Column::PayloadJson.is_not_null())
            .order_by_asc(sse_outbox::Column::Id)
            .limit(limit)
            .all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| SseOutbox {
                id: row.id,
                delivery_key: row.delivery_key,
                payload_json: row.payload_json,
                created_at: row.created_at,
            })
            .collect())
    }

    async fn mark_sse_outbox_sent(&self, id: i64) -> Result<()> {
        sse_outbox::Entity::delete_by_id(id).exec(&self.db).await?;
        Ok(())
    }
}

use super::DbStore;
use crate::entities::{room_members, room_state_events};
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::models::{RoomMember, RoomMetadata, RoomStateEvent};
use cumments_core::ports::RoomStore;
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

fn state_event_from_model(model: room_state_events::Model) -> RoomStateEvent {
    RoomStateEvent {
        event_id: model.event_id,
        room_id: model.room_id,
        event_type: model.event_type,
        state_key: model.state_key,
        sender: model.sender,
        origin_server_ts: model.origin_server_ts,
        content_json: serde_json::from_str(&model.content_json).unwrap_or(serde_json::Value::Null),
    }
}

#[async_trait]
impl RoomStore for DbStore {
    async fn save_member(&self, member: &RoomMember) -> Result<()> {
        let model = room_members::ActiveModel {
            room_id: Set(member.room_id.clone()),
            user_id: Set(member.user_id.clone()),
            display_name: Set(member.display_name.clone()),
            avatar_url: Set(member.avatar_url.clone()),
            membership: Set(member.membership.clone()),
            updated_at: Set(member.updated_at),
        };
        room_members::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    room_members::Column::RoomId,
                    room_members::Column::UserId,
                ])
                .update_columns([
                    room_members::Column::DisplayName,
                    room_members::Column::AvatarUrl,
                    room_members::Column::Membership,
                    room_members::Column::UpdatedAt,
                ])
                .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn get_member(&self, room_id: &str, user_id: &str) -> Result<Option<RoomMember>> {
        let model = room_members::Entity::find_by_id((room_id.to_string(), user_id.to_string()))
            .one(&self.db)
            .await?;
        Ok(model.map(|m| RoomMember {
            room_id: m.room_id,
            user_id: m.user_id,
            display_name: m.display_name,
            avatar_url: m.avatar_url,
            membership: m.membership,
            updated_at: m.updated_at,
        }))
    }

    async fn save_state_event(&self, event: &RoomStateEvent) -> Result<()> {
        let model = room_state_events::ActiveModel {
            event_id: Set(event.event_id.clone()),
            room_id: Set(event.room_id.clone()),
            event_type: Set(event.event_type.clone()),
            state_key: Set(event.state_key.clone()),
            sender: Set(event.sender.clone()),
            origin_server_ts: Set(event.origin_server_ts),
            content_json: Set(
                serde_json::to_string(&event.content_json).unwrap_or_else(|_| "null".to_string())
            ),
            created_at: Set(chrono::Utc::now()),
        };
        room_state_events::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(room_state_events::Column::EventId)
                    .update_columns([
                        room_state_events::Column::RoomId,
                        room_state_events::Column::EventType,
                        room_state_events::Column::StateKey,
                        room_state_events::Column::Sender,
                        room_state_events::Column::OriginServerTs,
                        room_state_events::Column::ContentJson,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn get_state_event(&self, event_id: &str) -> Result<Option<RoomStateEvent>> {
        let model = room_state_events::Entity::find_by_id(event_id)
            .one(&self.db)
            .await?;
        Ok(model.map(state_event_from_model))
    }

    async fn update_state_event_content(
        &self,
        event_id: &str,
        content: &serde_json::Value,
    ) -> Result<bool> {
        let result = room_state_events::Entity::update_many()
            .filter(room_state_events::Column::EventId.eq(event_id))
            .col_expr(
                room_state_events::Column::ContentJson,
                sea_orm::sea_query::Expr::value(
                    serde_json::to_string(content).unwrap_or_else(|_| "null".to_string()),
                ),
            )
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<RoomMetadata>> {
        let events = room_state_events::Entity::find()
            .filter(room_state_events::Column::RoomId.eq(room_id))
            .filter(room_state_events::Column::EventType.is_in([
                "m.room.name",
                "m.room.topic",
                "m.room.avatar",
            ]))
            .order_by_desc(room_state_events::Column::OriginServerTs)
            .order_by_desc(room_state_events::Column::EventId)
            .all(&self.db)
            .await?;

        let mut name = None;
        let mut topic = None;
        let mut avatar_url = None;
        for event in events {
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&event.content_json) else {
                continue;
            };
            match event.event_type.as_str() {
                "m.room.name" if name.is_none() => {
                    name = json
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
                "m.room.topic" if topic.is_none() => {
                    topic = json
                        .get("topic")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
                "m.room.avatar" if avatar_url.is_none() => {
                    avatar_url = json.get("url").and_then(|v| v.as_str()).map(str::to_string);
                }
                _ => {}
            }
        }

        let member_count = room_members::Entity::find()
            .filter(room_members::Column::RoomId.eq(room_id))
            .filter(room_members::Column::Membership.eq("join"))
            .count(&self.db)
            .await? as i64;

        Ok(Some(RoomMetadata {
            room_id: room_id.to_string(),
            name,
            topic,
            avatar_url,
            member_count,
        }))
    }

    async fn get_room_system_messages(
        &self,
        room_id: &str,
        limit: i64,
    ) -> Result<Vec<RoomStateEvent>> {
        let models = room_state_events::Entity::find()
            .filter(room_state_events::Column::RoomId.eq(room_id))
            .order_by_desc(room_state_events::Column::OriginServerTs)
            .order_by_desc(room_state_events::Column::EventId)
            .limit(limit.max(0) as u64)
            .all(&self.db)
            .await?;
        Ok(models.into_iter().map(state_event_from_model).collect())
    }
}

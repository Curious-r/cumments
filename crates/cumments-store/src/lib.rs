use anyhow::Result;
use async_trait::async_trait;
use cumments_core::intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent};
use cumments_core::models::{Comment, PostSlug, Site, SiteId};
use cumments_core::ports::{CommentStore, IntentStore, RegistryStore, SiteStore};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, SqlxSqliteConnector,
};
use sqlx::SqlitePool;

pub mod entities;

/// A SqliteStore implementation of the storage ports.
#[derive(Clone)]
pub struct SqliteStore {
    db: DatabaseConnection,
}

impl SqliteStore {
    /// Creates a new SqliteStore using an existing SqlitePool.
    pub fn new(pool: SqlitePool) -> Self {
        let db = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool);
        Self { db }
    }
}

use crate::entities::*;

#[async_trait]
impl IntentStore for SqliteStore {
    async fn save_post_intent(&self, intent: &PostCommentIntent) -> Result<()> {
        let payload = serde_json::to_string(intent)?;

        let active_model = intent_queue_post_comment::ActiveModel {
            payload: Set(payload),
            status: Set("pending".to_owned()),
            retry_count: Set(0),
            ..Default::default()
        };

        active_model.insert(&self.db).await?;

        Ok(())
    }

    async fn save_delete_intent(&self, intent: &DeleteCommentIntent) -> Result<()> {
        let payload = serde_json::to_string(intent)?;

        let active_model = intent_queue_delete_comment::ActiveModel {
            payload: Set(payload),
            status: Set("pending".to_owned()),
            target_event_id: Set(Some(intent.event_id.clone())),
            ..Default::default()
        };

        active_model.insert(&self.db).await?;

        Ok(())
    }

    async fn save_update_intent(&self, intent: &UpdateCommentIntent) -> Result<()> {
        let active_model = intent_queue_update_comment::ActiveModel {
            site_id: Set(intent.site_id.as_str().to_owned()),
            post_slug: Set(intent.post_slug.as_str().to_owned()),
            event_id: Set(intent.event_id.clone()),
            content: Set(intent.content.clone()),
            author_fingerprint: Set(intent.author_fingerprint.clone()),
            status: Set("pending".to_owned()),
            ..Default::default()
        };

        active_model.insert(&self.db).await?;

        Ok(())
    }

    async fn mark_post_intent_waiting_for_sync(&self, id: i64, event_id: &str) -> Result<()> {
        intent_queue_post_comment::Entity::update_many()
            .col_expr(
                intent_queue_post_comment::Column::Status,
                sea_orm::sea_query::Expr::value("waiting_for_sync"),
            )
            .col_expr(
                intent_queue_post_comment::Column::MatrixEventId,
                sea_orm::sea_query::Expr::value(event_id),
            )
            .col_expr(
                intent_queue_post_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_post_comment::Column::Id.eq(id as i32))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_update_intent_waiting_for_sync(&self, id: i64) -> Result<()> {
        intent_queue_update_comment::Entity::update_many()
            .col_expr(
                intent_queue_update_comment::Column::Status,
                sea_orm::sea_query::Expr::value("waiting_for_sync"),
            )
            .col_expr(
                intent_queue_update_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_update_comment::Column::Id.eq(id as i32))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_post_intent_completed(&self, event_id: &str) -> Result<()> {
        intent_queue_post_comment::Entity::update_many()
            .col_expr(
                intent_queue_post_comment::Column::Status,
                sea_orm::sea_query::Expr::value("completed"),
            )
            .col_expr(
                intent_queue_post_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_post_comment::Column::MatrixEventId.eq(event_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_delete_intent_completed(&self, target_event_id: &str) -> Result<()> {
        intent_queue_delete_comment::Entity::update_many()
            .col_expr(
                intent_queue_delete_comment::Column::Status,
                sea_orm::sea_query::Expr::value("completed"),
            )
            .col_expr(
                intent_queue_delete_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_delete_comment::Column::TargetEventId.eq(target_event_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_update_intent_completed(&self, event_id: &str) -> Result<()> {
        intent_queue_update_comment::Entity::update_many()
            .col_expr(
                intent_queue_update_comment::Column::Status,
                sea_orm::sea_query::Expr::value("completed"),
            )
            .col_expr(
                intent_queue_update_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_update_comment::Column::EventId.eq(event_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl CommentStore for SqliteStore {
    async fn get_comment(&self, event_id: &str) -> Result<Option<Comment>> {
        let model = comments::Entity::find()
            .filter(comments::Column::EventId.eq(event_id))
            .one(&self.db)
            .await?;

        Ok(model.map(Comment::from))
    }

    async fn get_comments(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Comment>, i64)> {
        let site_id_str = site_id.as_str();
        let post_slug_str = post_slug.as_str();

        let count = comments::Entity::find()
            .filter(comments::Column::SiteId.eq(site_id_str))
            .filter(comments::Column::PostSlug.eq(post_slug_str))
            .count(&self.db)
            .await?;

        let models = comments::Entity::find()
            .filter(comments::Column::SiteId.eq(site_id_str))
            .filter(comments::Column::PostSlug.eq(post_slug_str))
            .order_by_desc(comments::Column::Timestamp)
            .limit(limit as u64)
            .offset(offset as u64)
            .all(&self.db)
            .await?;

        let comments = models.into_iter().map(Comment::from).collect();

        Ok((comments, count as i64))
    }
}

#[async_trait]
impl RegistryStore for SqliteStore {
    async fn get_registered_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<Option<String>> {
        let room = room_registry::Entity::find()
            .filter(room_registry::Column::SiteId.eq(site_id.as_str()))
            .filter(room_registry::Column::PostSlug.eq(post_slug.as_str()))
            .filter(room_registry::Column::IsActive.eq(true))
            .one(&self.db)
            .await?;

        Ok(room.map(|r| r.room_id))
    }

    async fn invalidate_room_registry(&self, room_id: &str) -> Result<()> {
        room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::IsActive,
                sea_orm::sea_query::Expr::value(false),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(room_registry::Column::RoomId.eq(room_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl SiteStore for SqliteStore {
    async fn get_site(&self, id: &SiteId) -> Result<Option<Site>> {
        let model = sites::Entity::find_by_id(id.as_str().to_owned())
            .one(&self.db)
            .await?;

        Ok(model.map(Site::from))
    }

    async fn save_site(&self, site: &Site) -> Result<()> {
        let active_model = sites::ActiveModel {
            id: Set(site.id.clone()),
            matrix_space_id: Set(site.matrix_space_id.clone()),
            display_name: Set(site.display_name.clone()),
            created_at: Set(site.created_at),
        };

        sites::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(sites::Column::Id)
                    .update_columns([sites::Column::MatrixSpaceId, sites::Column::DisplayName])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }
}

impl From<comments::Model> for Comment {
    fn from(model: comments::Model) -> Self {
        Comment {
            event_id: model.event_id,
            author_nickname: model.author_nickname,
            author_fingerprint: model.author_fingerprint,
            content: model.content,
            timestamp: model.timestamp,
        }
    }
}

impl From<sites::Model> for Site {
    fn from(model: sites::Model) -> Self {
        Site {
            id: model.id,
            matrix_space_id: model.matrix_space_id,
            display_name: model.display_name,
            created_at: model.created_at,
        }
    }
}

use anyhow::Result;
use async_trait::async_trait;
use cumments_core::intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent};
use cumments_core::models::{Comment, PostSlug, Site, SiteId};
use cumments_core::ports::{CommentStore, IntentStore, RegistryStore, SiteStore};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};

pub mod entities;
pub mod migration;

/// A database-backed implementation of the storage ports.
#[derive(Clone)]
pub struct DbStore {
    db: DatabaseConnection,
}

impl DbStore {
    /// Creates a new DbStore by connecting to the database at the given URL.
    pub async fn connect(url: &str) -> Result<Self> {
        use migration::MigratorTrait;
        let db = sea_orm::Database::connect(url).await?;
        migration::Migrator::up(&db, None).await?;
        Ok(Self { db })
    }
}

use crate::entities::*;

#[async_trait]
impl IntentStore for DbStore {
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

    async fn get_pending_post_intents(&self) -> Result<Vec<(i64, PostCommentIntent)>> {
        let models = intent_queue_post_comment::Entity::find()
            .filter(intent_queue_post_comment::Column::Status.eq("pending"))
            .order_by_asc(intent_queue_post_comment::Column::CreatedAt)
            .all(&self.db)
            .await?;

        let mut intents = Vec::new();
        for m in models {
            let intent: PostCommentIntent = serde_json::from_str(&m.payload)?;
            intents.push((m.id as i64, intent));
        }
        Ok(intents)
    }

    async fn get_pending_delete_intents(&self) -> Result<Vec<(i64, DeleteCommentIntent)>> {
        let models = intent_queue_delete_comment::Entity::find()
            .filter(intent_queue_delete_comment::Column::Status.eq("pending"))
            .order_by_asc(intent_queue_delete_comment::Column::CreatedAt)
            .all(&self.db)
            .await?;

        let mut intents = Vec::new();
        for m in models {
            let intent: DeleteCommentIntent = serde_json::from_str(&m.payload)?;
            intents.push((m.id as i64, intent));
        }
        Ok(intents)
    }

    async fn get_pending_update_intents(&self) -> Result<Vec<(i64, UpdateCommentIntent)>> {
        let models = intent_queue_update_comment::Entity::find()
            .filter(intent_queue_update_comment::Column::Status.eq("pending"))
            .order_by_asc(intent_queue_update_comment::Column::CreatedAt)
            .all(&self.db)
            .await?;

        let mut intents = Vec::new();
        for m in models {
            let intent = UpdateCommentIntent {
                site_id: m.site_id.into(),
                post_slug: m.post_slug.into(),
                event_id: m.event_id,
                content: m.content,
                author_fingerprint: m.author_fingerprint,
            };
            intents.push((m.id as i64, intent));
        }
        Ok(intents)
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
            .filter(intent_queue_post_comment::Column::Id.eq(id))
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
            .filter(intent_queue_update_comment::Column::Id.eq(id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_delete_intent_waiting_for_sync(&self, id: i64) -> Result<()> {
        intent_queue_delete_comment::Entity::update_many()
            .col_expr(
                intent_queue_delete_comment::Column::Status,
                sea_orm::sea_query::Expr::value("waiting_for_sync"),
            )
            .col_expr(
                intent_queue_delete_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_delete_comment::Column::Id.eq(id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_post_intent_failed(&self, id: i64) -> Result<()> {
        intent_queue_post_comment::Entity::update_many()
            .col_expr(
                intent_queue_post_comment::Column::Status,
                sea_orm::sea_query::Expr::value("failed"),
            )
            .col_expr(
                intent_queue_post_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_post_comment::Column::Id.eq(id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_delete_intent_failed(&self, id: i64) -> Result<()> {
        intent_queue_delete_comment::Entity::update_many()
            .col_expr(
                intent_queue_delete_comment::Column::Status,
                sea_orm::sea_query::Expr::value("failed"),
            )
            .col_expr(
                intent_queue_delete_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_delete_comment::Column::Id.eq(id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_update_intent_failed(&self, id: i64) -> Result<()> {
        intent_queue_update_comment::Entity::update_many()
            .col_expr(
                intent_queue_update_comment::Column::Status,
                sea_orm::sea_query::Expr::value("failed"),
            )
            .col_expr(
                intent_queue_update_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(intent_queue_update_comment::Column::Id.eq(id))
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
impl CommentStore for DbStore {
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

    async fn save_comment(
        &self,
        comment: &Comment,
        room_id: &str,
        _site_id: &SiteId,
        _post_slug: &PostSlug,
    ) -> Result<()> {
        let active_model = comments::ActiveModel {
            event_id: Set(comment.event_id.clone()),
            room_id: Set(room_id.to_owned()),
            site_id: Set(comment.site_id.clone()),
            post_slug: Set(comment.post_slug.clone()),
            author_mxid: Set("".to_string()), // Default value if not provided
            author_nickname: Set(comment.author_nickname.clone()),
            author_fingerprint: Set(comment.author_fingerprint.clone()),
            content: Set(comment.content.clone()),
            timestamp: Set(comment.timestamp),
            ..Default::default()
        };

        comments::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(comments::Column::EventId)
                    .update_columns([
                        comments::Column::AuthorNickname,
                        comments::Column::Content,
                        comments::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn update_comment_content(&self, event_id: &str, content: &str) -> Result<bool> {
        let result = comments::Entity::update_many()
            .col_expr(
                comments::Column::Content,
                sea_orm::sea_query::Expr::value(content),
            )
            .col_expr(
                comments::Column::UpdatedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(comments::Column::EventId.eq(event_id))
            .exec(&self.db)
            .await?;

        Ok(result.rows_affected > 0)
    }

    async fn delete_comment(&self, event_id: &str) -> Result<bool> {
        let result = comments::Entity::delete_many()
            .filter(comments::Column::EventId.eq(event_id))
            .exec(&self.db)
            .await?;

        Ok(result.rows_affected > 0)
    }

    async fn get_author_nickname(&self, event_id: &str) -> Result<Option<String>> {
        let model = comments::Entity::find()
            .filter(comments::Column::EventId.eq(event_id))
            .one(&self.db)
            .await?;

        Ok(model.and_then(|m| m.author_nickname))
    }
}

#[async_trait]
impl RegistryStore for DbStore {
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

    async fn is_room_active(&self, room_id: &str) -> Result<Option<bool>> {
        let room = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;

        Ok(room.map(|r| r.is_active))
    }

    async fn register_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<()> {
        let active_model = room_registry::ActiveModel {
            room_id: Set(room_id.to_owned()),
            site_id: Set(site_id.as_str().to_owned()),
            post_slug: Set(post_slug.as_str().to_owned()),
            is_active: Set(true),
            ..Default::default()
        };

        room_registry::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(room_registry::Column::RoomId)
                    .update_column(room_registry::Column::IsActive)
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
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
impl SiteStore for DbStore {
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

    async fn ensure_site_exists(&self, site_id: &str, matrix_space_id: &str) -> Result<()> {
        let active_model = sites::ActiveModel {
            id: Set(site_id.to_owned()),
            matrix_space_id: Set(matrix_space_id.to_owned()),
            display_name: Set(Some(site_id.to_owned())),
            ..Default::default()
        };

        sites::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(sites::Column::Id)
                    .do_nothing()
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
            site_id: model.site_id,
            post_slug: model.post_slug,
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

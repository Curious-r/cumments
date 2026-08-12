use super::DbStore;
use crate::entities::backfill_tombstones;
use crate::entities::comments;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::models::{AuthorType, Comment, CommentAuthor, CommentPage, PostSlug, SiteId};
use cumments_core::ports::CommentStore;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

#[async_trait]
impl CommentStore for DbStore {
    async fn get_comment(&self, event_id: &str) -> Result<Option<Comment>> {
        let model = comments::Entity::find()
            .filter(comments::COLUMN.event_id.eq(event_id))
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
    ) -> Result<CommentPage> {
        let site_id_str = site_id.as_str();
        let post_slug_str = post_slug.as_str();

        let query = comments::Entity::find()
            .filter(comments::COLUMN.site_id.eq(site_id_str))
            .filter(comments::COLUMN.post_slug.eq(post_slug_str))
            .order_by_desc(comments::Column::Timestamp)
            .order_by_asc(comments::Column::EventId);

        let count = query.clone().count(&self.db).await?;
        if limit <= 0 {
            return Ok(CommentPage {
                items: Vec::new(),
                total: count as i64,
            });
        }

        let models = query
            .offset(offset.max(0) as u64)
            .limit(limit as u64)
            .all(&self.db)
            .await?;

        let comments = models.into_iter().map(Comment::from).collect();

        Ok(CommentPage {
            items: comments,
            total: count as i64,
        })
    }

    async fn save_comment(
        &self,
        comment: &Comment,
        room_id: &str,
        sender: &str,
        _site_id: &SiteId,
        _post_slug: &PostSlug,
    ) -> Result<()> {
        let active_model = comments::ActiveModel {
            event_id: Set(comment.event_id.clone()),
            room_id: Set(room_id.to_owned()),
            site_id: Set(comment.site_id.clone()),
            post_slug: Set(comment.post_slug.clone()),
            sender_mxid: Set(sender.to_owned()),
            author_type: Set(comment.author.kind.as_str().to_string()),
            author_display_name: Set(comment.author.display_name.clone()),
            author_public_key: Set(comment.author.public_key.clone()),
            content: Set(comment.content.clone()),
            timestamp: Set(comment.timestamp),
            reply_to: Set(comment.reply_to.clone()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        comments::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(comments::Column::EventId)
                    .update_columns([
                        comments::Column::RoomId,
                        comments::Column::SiteId,
                        comments::Column::PostSlug,
                        comments::Column::SenderMxid,
                        comments::Column::AuthorType,
                        comments::Column::AuthorDisplayName,
                        comments::Column::AuthorPublicKey,
                        comments::Column::Content,
                        comments::Column::Timestamp,
                        comments::Column::ReplyTo,
                        comments::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn update_comment_content(
        &self,
        event_id: &str,
        room_id: &str,
        content: &str,
        edit_ts: i64,
        edit_event_id: &str,
    ) -> Result<bool> {
        // Only apply when the edit is newer than the last applied edit
        // (missing last_edit means the original is current), with event_id
        // as the deterministic tie-breaker.
        let recency = Condition::any()
            .add(comments::Column::LastEditTs.is_null())
            .add(comments::Column::LastEditTs.lt(edit_ts))
            .add(
                Condition::all()
                    .add(comments::Column::LastEditTs.eq(edit_ts))
                    .add(comments::Column::LastEditEventId.lt(edit_event_id)),
            );

        let result = comments::Entity::update_many()
            .col_expr(
                comments::Column::Content,
                sea_orm::sea_query::Expr::value(content),
            )
            .col_expr(
                comments::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .col_expr(
                comments::Column::LastEditTs,
                sea_orm::sea_query::Expr::value(edit_ts),
            )
            .col_expr(
                comments::Column::LastEditEventId,
                sea_orm::sea_query::Expr::value(edit_event_id),
            )
            .filter(comments::COLUMN.event_id.eq(event_id))
            .filter(comments::COLUMN.room_id.eq(room_id))
            .filter(recency)
            .exec(&self.db)
            .await?;

        Ok(result.rows_affected > 0)
    }

    async fn delete_comment(&self, event_id: &str) -> Result<bool> {
        let result = comments::Entity::delete_many()
            .filter(comments::COLUMN.event_id.eq(event_id))
            .exec(&self.db)
            .await?;

        Ok(result.rows_affected > 0)
    }

    async fn get_author_display_name(&self, event_id: &str) -> Result<Option<Option<String>>> {
        let model = comments::Entity::find()
            .filter(comments::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;

        Ok(model.map(|m| m.author_display_name))
    }

    async fn get_comment_author_public_key(&self, event_id: &str) -> Result<Option<String>> {
        let model = comments::Entity::find()
            .filter(comments::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;

        Ok(model.and_then(|m| m.author_public_key))
    }

    async fn record_backfill_tombstone(
        &self,
        event_id: &str,
        room_id: &str,
        redaction_event_id: &str,
    ) -> Result<()> {
        let active_model = backfill_tombstones::ActiveModel {
            event_id: Set(event_id.to_owned()),
            room_id: Set(room_id.to_owned()),
            redaction_event_id: Set(redaction_event_id.to_owned()),
            created_at: Set(chrono::Utc::now()),
        };

        backfill_tombstones::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(backfill_tombstones::Column::EventId)
                    .update_column(backfill_tombstones::Column::RoomId)
                    .update_column(backfill_tombstones::Column::RedactionEventId)
                    .update_column(backfill_tombstones::Column::CreatedAt)
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn has_backfill_tombstone(&self, event_id: &str, room_id: &str) -> Result<bool> {
        let found = backfill_tombstones::Entity::find()
            .filter(backfill_tombstones::COLUMN.event_id.eq(event_id))
            .filter(backfill_tombstones::COLUMN.room_id.eq(room_id))
            .one(&self.db)
            .await?;
        Ok(found.is_some())
    }
}

impl From<comments::Model> for Comment {
    fn from(model: comments::Model) -> Self {
        Comment {
            event_id: model.event_id,
            site_id: model.site_id,
            post_slug: model.post_slug,
            author: CommentAuthor {
                kind: AuthorType::from_db(&model.author_type, model.author_public_key.is_some()),
                display_name: model.author_display_name,
                public_key: model.author_public_key,
                mxid: if model.author_type == "matrix" {
                    Some(model.sender_mxid.clone())
                } else {
                    None
                },
            },
            content: model.content,
            timestamp: model.timestamp,
            reply_to: model.reply_to,
            room_id: model.room_id,
            sender_mxid: model.sender_mxid,
        }
    }
}

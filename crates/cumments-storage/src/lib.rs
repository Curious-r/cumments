use anyhow::Result;
use async_trait::async_trait;
use cumments_core::intents::{DeleteCommentIntent, PostCommentIntent};
use cumments_core::models::Comment;
use cumments_core::ports::{CommentRepository, IntentRepository};
use sqlx::SqlitePool;

/// A wrapper around the database pool that provides concrete implementations
/// of the storage-related ports defined in `cumments-core`.
pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl IntentRepository for Storage {
    /// Saves a `PostCommentIntent` to the `intent_queue_post_comment` table.
    /// The intent is serialized to JSON for storage. The Reconciler will
    /// later deserialize it for processing.
    async fn save_post_comment_intent(&self, intent: &PostCommentIntent) -> Result<()> {
        let intent_json = serde_json::to_string(intent)?;

        sqlx::query!(
            r#"
            INSERT INTO intent_queue_post_comment (payload)
            VALUES (?)
            "#,
            intent_json
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn save_delete_comment_intent(&self, intent: &DeleteCommentIntent) -> Result<()> {
        let intent_json = serde_json::to_string(intent)?;

        sqlx::query!(
            r#"
            INSERT INTO intent_queue_delete_comment (payload)
            VALUES (?)
            "#,
            intent_json
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

#[async_trait]
impl CommentRepository for Storage {
    async fn get_comments(
        &self,
        site_id: &str,
        post_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Comment>, i64)> {
        let count_query = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) as "count: i64"
            FROM comments
            WHERE site_id = ? AND post_slug = ?
            "#,
            site_id,
            post_slug
        )
        .fetch_one(&self.pool);

        let comments_query = sqlx::query_as!(
            Comment,
            r#"
            SELECT event_id, author_nickname, content, timestamp
            FROM comments
            WHERE site_id = ? AND post_slug = ?
            ORDER BY timestamp ASC
            LIMIT ? OFFSET ?
            "#,
            site_id,
            post_slug,
            limit,
            offset
        )
        .fetch_all(&self.pool);

        let (total, comments) = tokio::try_join!(count_query, comments_query)?;

        Ok((comments, total.unwrap_or(0)))
    }
}

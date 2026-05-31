use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use cumments_core::intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent};
use cumments_core::models::{Comment, PostSlug, Site, SiteId};
use cumments_core::ports::{CommentStore, IntentStore, RegistryStore, SiteStore};
use sqlx::SqlitePool;

/// A SqliteStore implementation of the storage ports.
#[derive(Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Creates a new SqliteStore using an existing SqlitePool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl IntentStore for SqliteStore {
    async fn save_post_intent(&self, intent: &PostCommentIntent) -> Result<()> {
        let payload = serde_json::to_string(intent)?;

        sqlx::query!(
            r#"
            INSERT INTO intent_queue_post_comment (payload)
            VALUES (?)
            "#,
            payload
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn save_delete_intent(&self, intent: &DeleteCommentIntent) -> Result<()> {
        let payload = serde_json::to_string(intent)?;

        sqlx::query!(
            r#"
            INSERT INTO intent_queue_delete_comment (payload, target_event_id)
            VALUES (?, ?)
            "#,
            payload,
            intent.event_id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn save_update_intent(&self, intent: &UpdateCommentIntent) -> Result<()> {
        let site_id = intent.site_id.as_str();
        let post_slug = intent.post_slug.as_str();

        sqlx::query!(
            r#"
            INSERT INTO intent_queue_update_comment (site_id, post_slug, event_id, content, author_fingerprint)
            VALUES (?, ?, ?, ?, ?)
            "#,
            site_id,
            post_slug,
            intent.event_id,
            intent.content,
            intent.author_fingerprint
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn mark_post_intent_waiting_for_sync(&self, id: i64, event_id: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE intent_queue_post_comment SET status = 'waiting_for_sync', matrix_event_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            event_id,
            id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_update_intent_waiting_for_sync(&self, id: i64) -> Result<()> {
        sqlx::query!(
            "UPDATE intent_queue_update_comment SET status = 'waiting_for_sync', updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_post_intent_completed(&self, event_id: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE intent_queue_post_comment SET status = 'completed', updated_at = CURRENT_TIMESTAMP WHERE matrix_event_id = ?",
            event_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_delete_intent_completed(&self, target_event_id: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE intent_queue_delete_comment SET status = 'completed', updated_at = CURRENT_TIMESTAMP WHERE target_event_id = ?",
            target_event_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_update_intent_completed(&self, event_id: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE intent_queue_update_comment SET status = 'completed', updated_at = CURRENT_TIMESTAMP WHERE event_id = ?",
            event_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl CommentStore for SqliteStore {
    async fn get_comment(&self, event_id: &str) -> Result<Option<Comment>> {
        let row = sqlx::query_as!(
            CommentRow,
            r#"
            SELECT event_id, author_nickname, author_fingerprint, content, timestamp as "timestamp: NaiveDateTime"
            FROM comments
            WHERE event_id = ?
            "#,
            event_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Comment::from))
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

        let count_query = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) as "count: i64"
            FROM comments
            WHERE site_id = ? AND post_slug = ?
            "#,
            site_id_str,
            post_slug_str
        )
        .fetch_one(&self.pool);

        let site_id_str = site_id.as_str();
        let post_slug_str = post_slug.as_str();

        let comments_query = sqlx::query_as!(
            CommentRow,
            r#"
            SELECT event_id, author_nickname, author_fingerprint, content, timestamp as "timestamp: NaiveDateTime"
            FROM comments
            WHERE site_id = ? AND post_slug = ?
            ORDER BY timestamp DESC
            LIMIT ? OFFSET ?
            "#,
            site_id_str,
            post_slug_str,
            limit,
            offset
        )
        .fetch_all(&self.pool);

        let (total, comment_rows) = tokio::try_join!(count_query, comments_query)?;

        let comments = comment_rows.into_iter().map(Comment::from).collect();

        Ok((comments, total))
    }
}

#[async_trait]
impl RegistryStore for SqliteStore {
    async fn get_registered_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<Option<String>> {
        let s_id = site_id.as_str();
        let p_slug = post_slug.as_str();
        let room_id = sqlx::query_scalar!(
            "SELECT room_id FROM room_registry WHERE site_id = ? AND post_slug = ? AND is_active = 1",
            s_id,
            p_slug
        )
        .fetch_optional(&self.pool)
        .await?
        .flatten();

        Ok(room_id)
    }

    async fn invalidate_room_registry(&self, room_id: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE room_registry SET is_active = 0, updated_at = CURRENT_TIMESTAMP WHERE room_id = ?",
            room_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl SiteStore for SqliteStore {
    async fn get_site(&self, id: &SiteId) -> Result<Option<Site>> {
        let id_str = id.as_str();

        let row = sqlx::query_as!(
            SiteRow,
            r#"
            SELECT id as "id!", matrix_space_id as "matrix_space_id!", display_name, created_at as "created_at!: NaiveDateTime"
            FROM sites
            WHERE id = ?
            "#,
            id_str
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Site::from))
    }

    async fn save_site(&self, site: &Site) -> Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO sites (id, matrix_space_id, display_name, created_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                matrix_space_id = excluded.matrix_space_id,
                display_name = excluded.display_name
            "#,
            site.id,
            site.matrix_space_id,
            site.display_name,
            site.created_at
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

// An intermediate struct for reading from the database, to handle the
// NaiveDateTime -> DateTime<Utc> conversion.
#[derive(sqlx::FromRow)]
struct CommentRow {
    event_id: String,
    author_nickname: Option<String>,
    author_fingerprint: Option<String>,
    content: String,
    timestamp: NaiveDateTime,
}

impl From<CommentRow> for Comment {
    fn from(row: CommentRow) -> Self {
        Comment {
            event_id: row.event_id,
            author_nickname: row.author_nickname,
            author_fingerprint: row.author_fingerprint,
            content: row.content,
            timestamp: DateTime::from_naive_utc_and_offset(row.timestamp, Utc),
        }
    }
}

#[derive(sqlx::FromRow)]
struct SiteRow {
    id: String,
    matrix_space_id: String,
    display_name: Option<String>,
    created_at: NaiveDateTime,
}

impl From<SiteRow> for Site {
    fn from(row: SiteRow) -> Self {
        Site {
            id: row.id,
            matrix_space_id: row.matrix_space_id,
            display_name: row.display_name,
            created_at: DateTime::from_naive_utc_and_offset(row.created_at, Utc),
        }
    }
}

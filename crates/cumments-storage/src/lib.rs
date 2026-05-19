use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use cumments_core::intents::{DeleteCommentIntent, PostCommentIntent};
use cumments_core::models::{Comment, Site};
use cumments_core::ports::{CommentRepository, IntentRepository, SiteRepository};
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

// An intermediate struct for reading from the database, to handle the
// NaiveDateTime -> DateTime<Utc> conversion.
#[derive(sqlx::FromRow)]
struct CommentRow {
    event_id: String,
    author_nickname: Option<String>,
    content: String,
    timestamp: NaiveDateTime,
}

impl From<CommentRow> for Comment {
    fn from(row: CommentRow) -> Self {
        Comment {
            event_id: row.event_id,
            author_nickname: row.author_nickname,
            content: row.content,
            timestamp: DateTime::from_naive_utc_and_offset(row.timestamp, Utc),
        }
    }
}

// An intermediate struct for reading sites from the database.
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
            CommentRow,
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

        let (total, comment_rows) = tokio::try_join!(count_query, comments_query)?;

        let comments = comment_rows.into_iter().map(Comment::from).collect();

        Ok((comments, total))
    }
}

#[async_trait]
impl SiteRepository for Storage {
    async fn get_site(&self, id: &str) -> Result<Option<Site>> {
        let site_row = sqlx::query_as!(
            SiteRow,
            r#"
            SELECT id as "id!", matrix_space_id as "matrix_space_id!", display_name, created_at as "created_at!: NaiveDateTime"
            FROM sites
            WHERE id = ?
            "#,
            id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(site_row.map(Site::from))
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

use chrono::NaiveDateTime;
use domain::{Comment, SiteId};
use sqlx::{migrate::MigrateDatabase, sqlite::SqlitePoolOptions, FromRow, Pool, Sqlite};
use std::fs;
use std::path::Path;

// --- Internal Data Model (POCO) ---
#[derive(FromRow)]
struct SqlComment {
    id: String,
    site_id: String,
    post_slug: String,
    author_id: String,
    author_name: String,
    is_guest: bool,
    is_redacted: bool,
    content: String,
    created_at: NaiveDateTime,
    reply_to: Option<String>,
    updated_at: Option<NaiveDateTime>,
}

// --- Mapper ---
impl From<SqlComment> for Comment {
    fn from(sql: SqlComment) -> Self {
        Comment {
            id: sql.id,
            site_id: SiteId::new_unchecked(sql.site_id),
            post_slug: sql.post_slug,
            author_id: sql.author_id,
            author_name: sql.author_name,
            is_guest: sql.is_guest,
            is_redacted: sql.is_redacted,
            content: sql.content,
            created_at: sql.created_at,
            reply_to: sql.reply_to,
            updated_at: sql.updated_at,
        }
    }
}

#[derive(Clone)]
pub struct Db {
    pool: Pool<Sqlite>,
}

impl Db {
    pub async fn new(db_url: &str) -> anyhow::Result<Self> {
        if db_url.starts_with("sqlite://") && !db_url.contains(":memory:") {
            let path_str = db_url.trim_start_matches("sqlite://");
            let path = Path::new(path_str);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    tracing::info!("Creating database directory: {:?}", parent);
                    fs::create_dir_all(parent)?;
                }
            }
        }

        if !Sqlite::database_exists(db_url).await.unwrap_or(false) {
            tracing::info!("Database file not found, creating: {}", db_url);
            Sqlite::create_database(db_url).await?;
        }

        let pool = SqlitePoolOptions::new().connect(db_url).await?;

        sqlx::query("PRAGMA journal_mode = WAL;")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA synchronous = NORMAL;")
            .execute(&pool)
            .await?;

        sqlx::migrate!("../../migrations").run(&pool).await?;

        Ok(Self { pool })
    }

    pub async fn upsert_comment(&self, c: &Comment) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO comments (
                id, site_id, post_slug, author_id, author_name,
                is_guest, is_redacted, content, created_at, reply_to,
                updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                content = excluded.content,
                is_redacted = excluded.is_redacted,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&c.id)
        .bind(c.site_id.as_str())
        .bind(&c.post_slug)
        .bind(&c.author_id)
        .bind(&c.author_name)
        .bind(c.is_guest)
        .bind(c.is_redacted)
        .bind(&c.content)
        .bind(c.created_at)
        .bind(&c.reply_to)
        .bind(c.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_comment(&self, id: &str) -> anyhow::Result<Option<(SiteId, String)>> {
        struct DeletedMeta {
            site_id: String,
            post_slug: String,
        }

        let row = sqlx::query_as!(
            DeletedMeta,
            r#"
                UPDATE comments
                SET
                    content = '',
                    author_name = '[Deleted]',
                    is_redacted = TRUE
                WHERE id = ?
                RETURNING site_id, post_slug
                "#,
            id
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some((SiteId::new_unchecked(r.site_id), r.post_slug))),
            None => Ok(None),
        }
    }

    pub async fn list_comments(&self, site_id: &str, slug: &str) -> anyhow::Result<Vec<Comment>> {
        let rows = sqlx::query_as!(
            SqlComment,
            r#"
            SELECT
                id, site_id, post_slug, author_id, author_name,
                is_guest, is_redacted, content, created_at, reply_to,
                updated_at
            FROM comments
            WHERE site_id = ? AND post_slug = ?
            ORDER BY created_at ASC
            "#,
            site_id,
            slug
        )
        .fetch_all(&self.pool)
        .await?;

        let comments = rows.into_iter().map(Comment::from).collect();
        Ok(comments)
    }

    pub async fn get_sync_token(&self) -> anyhow::Result<Option<String>> {
        use sqlx::Row;
        let row = sqlx::query("SELECT value FROM meta WHERE key = 'sync_token'")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get(0)))
    }

    pub async fn save_sync_token(&self, token: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO meta (key, value) VALUES ('sync_token', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value"
        )
        .bind(token)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

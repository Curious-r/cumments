use chrono::NaiveDateTime;
use domain::{Comment, SiteId};
use sqlx::{migrate::MigrateDatabase, sqlite::SqlitePoolOptions, FromRow, Pool, Sqlite};
use std::{fs, path::Path};

#[derive(FromRow)]
struct SqlComment {
    id: String,
    author_id: String,
    author_name: String,
    is_guest: bool,
    is_redacted: bool,
    content: String,
    created_at: NaiveDateTime,
    updated_at: Option<NaiveDateTime>,
    reply_to: Option<String>,

    site_id: String,
    post_slug: String,
}

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
            updated_at: sql.updated_at,
            reply_to: sql.reply_to,
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
                    fs::create_dir_all(parent)?;
                }
            }
        }

        if !Sqlite::database_exists(db_url).await.unwrap_or(false) {
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

    pub async fn ensure_room(
        &self,
        room_id: &str,
        site_id: &str,
        slug: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO rooms (room_id, site_id, post_slug)
            VALUES (?, ?, ?)
            ON CONFLICT(room_id) DO NOTHING
            -- 注意：如果 (site_id, slug) 冲突，这里会报错，这正是我们要的强一致性
            "#,
        )
        .bind(room_id)
        .bind(site_id)
        .bind(slug)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_comment(&self, room_id: &str, c: &Comment) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO comments (
                id, room_id, author_id, author_name,
                is_guest, is_redacted, content, created_at, updated_at, reply_to
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                content = excluded.content,
                is_redacted = excluded.is_redacted,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&c.id)
        .bind(room_id)
        .bind(&c.author_id)
        .bind(&c.author_name)
        .bind(c.is_guest)
        .bind(c.is_redacted)
        .bind(&c.content)
        .bind(c.created_at)
        .bind(c.updated_at)
        .bind(&c.reply_to)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_comment(&self, id: &str) -> anyhow::Result<Option<(SiteId, String)>> {
        let mut tx = self.pool.begin().await?;

        let meta = sqlx::query!(
            r#"
            SELECT r.site_id, r.post_slug
            FROM comments c
            JOIN rooms r ON c.room_id = r.room_id
            WHERE c.id = ?
            "#,
            id
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(m) = meta {
            sqlx::query(
                r#"
                UPDATE comments
                SET content = '', author_name = '[Deleted]', is_redacted = TRUE
                WHERE id = ?
                "#,
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(Some((SiteId::new_unchecked(m.site_id), m.post_slug)))
        } else {
            Ok(None)
        }
    }

    pub async fn list_comments(&self, site_id: &str, slug: &str) -> anyhow::Result<Vec<Comment>> {
        let rows = sqlx::query_as!(
            SqlComment,
            r#"
                SELECT
                    c.id as "id!",
                    c.author_id as "author_id!",
                    c.author_name as "author_name!",
                    c.is_guest,
                    c.is_redacted,
                    c.content as "content!",
                    c.created_at,
                    c.updated_at,
                    c.reply_to,
                    r.site_id as "site_id!",
                    r.post_slug as "post_slug!"
                FROM comments c
                JOIN rooms r ON c.room_id = r.room_id
                WHERE r.site_id = ? AND r.post_slug = ?
                ORDER BY c.created_at ASC
                "#,
            site_id,
            slug
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Comment::from).collect())
    }

    pub async fn get_room_meta(&self, room_id: &str) -> anyhow::Result<Option<(SiteId, String)>> {
        let row = sqlx::query!(
            "SELECT site_id, post_slug FROM rooms WHERE room_id = ?",
            room_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| (SiteId::new_unchecked(r.site_id), r.post_slug)))
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

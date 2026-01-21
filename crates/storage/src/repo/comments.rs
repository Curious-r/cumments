use crate::{models::SqlComment, Db};
use domain::{Comment, SiteId};

impl Db {
    pub async fn upsert_comment(
        &self,
        room_id: &str,
        site_id: &str,
        slug: &str,
        c: &Comment,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            INSERT OR IGNORE INTO rooms (room_id, site_id, post_slug)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(room_id)
        .bind(site_id)
        .bind(slug)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO comments (
                id, room_id, author_id, author_name,
                is_guest, is_redacted,
                author_fingerprint,
                content, created_at, updated_at, reply_to
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(&c.author_fingerprint)
        .bind(&c.content)
        .bind(c.created_at)
        .bind(c.updated_at)
        .bind(&c.reply_to)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
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
                c.author_fingerprint,
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
}

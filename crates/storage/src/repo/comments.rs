use crate::{models::SqlComment, Db};
use domain::{Comment, SiteId};

impl Db {
    // 写入评论 (包含新字段)
    // raw_event_json: 为了数据韧性，允许存入原始 JSON 字符串
    pub async fn upsert_comment(
        &self,
        room_id: &str,
        site_id: &str,
        slug: &str,
        c: &Comment,
        raw_event_json: Option<String>,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;

        // 1. 确保 Room 映射存在 (防御性编程)
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

        // 2. 插入或更新评论
        sqlx::query(
            r#"
            INSERT INTO comments (
                id, room_id, author_id, author_name,
                is_guest, is_redacted,
                author_fingerprint, avatar_url,
                content, created_at, updated_at, reply_to,
                txn_id, raw_event
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                content = excluded.content,
                is_redacted = excluded.is_redacted,
                updated_at = excluded.updated_at,
                author_name = excluded.author_name, -- 允许更新作者名(如原生用户改名)
                avatar_url = excluded.avatar_url    -- 允许更新头像
            "#,
        )
        .bind(&c.id)
        .bind(room_id)
        .bind(&c.author_id)
        .bind(&c.author_name)
        .bind(c.is_guest)
        .bind(c.is_redacted)
        .bind(&c.author_fingerprint)
        .bind(&c.avatar_url)
        .bind(&c.content)
        .bind(c.created_at)
        .bind(c.updated_at)
        .bind(&c.reply_to)
        .bind(&c.txn_id)
        .bind(raw_event_json)
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
            // 软删除：保留 ID 以维持评论树结构，但清空内容
            sqlx::query(
                r#"
                UPDATE comments
                SET content = '', author_name = '[Deleted]', is_redacted = TRUE, avatar_url = NULL
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

    pub async fn get_comment(&self, comment_id: &str) -> anyhow::Result<Option<domain::Comment>> {
        let row = sqlx::query_as!(
            SqlComment,
            r#"
            SELECT
                c.id as "id!",
                c.author_id as "author_id!",
                c.author_name as "author_name!",
                c.is_guest,
                c.is_redacted,
                c.author_fingerprint,
                c.avatar_url,
                c.content as "content!",
                c.created_at,
                c.updated_at,
                c.reply_to,
                c.txn_id,
                r.site_id as "site_id!",
                r.post_slug as "post_slug!"
            FROM comments c
            JOIN rooms r ON c.room_id = r.room_id
            WHERE c.id = ?
            "#,
            comment_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn list_comments(
        &self,
        site_id: &str,
        slug: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<(Vec<Comment>, i64)> {
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
                c.avatar_url,
                c.content as "content!",
                c.created_at,
                c.updated_at,
                c.reply_to,
                c.txn_id,
                r.site_id as "site_id!",
                r.post_slug as "post_slug!"
            FROM comments c
            JOIN rooms r ON c.room_id = r.room_id
            WHERE r.site_id = ? AND r.post_slug = ?
            ORDER BY c.created_at ASC
            LIMIT ? OFFSET ?
            "#,
            site_id,
            slug,
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await?;

        // count 查询通常返回非空，不需要修改
        let count_row = sqlx::query!(
            r#"
            SELECT COUNT(*) as count
            FROM comments c
            JOIN rooms r ON c.room_id = r.room_id
            WHERE r.site_id = ? AND r.post_slug = ?
            "#,
            site_id,
            slug
        )
        .fetch_one(&self.pool)
        .await?;

        let comments = rows.into_iter().map(Into::into).collect();
        Ok((comments, count_row.count.into()))
    }
}

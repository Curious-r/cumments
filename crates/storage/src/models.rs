use chrono::NaiveDateTime;
use domain::{Comment, SiteId};
use sqlx::FromRow;

#[derive(FromRow)]
pub struct SqlComment {
    pub id: String,
    pub author_id: String,
    pub author_name: String,
    pub is_guest: bool,
    pub is_redacted: bool,
    pub author_fingerprint: Option<String>,
    pub avatar_url: Option<String>, // 新增
    pub content: String,
    pub created_at: NaiveDateTime,
    pub updated_at: Option<NaiveDateTime>,
    pub reply_to: Option<String>,
    pub txn_id: Option<String>,     // 新增

    // Join 字段 (来自 rooms 表)
    pub site_id: String,
    pub post_slug: String,
}

impl From<SqlComment> for Comment {
    fn from(sql: SqlComment) -> Self {
        Comment {
            id: sql.id,
            site_id: SiteId::new_unchecked(sql.site_id),
            post_slug: sql.post_slug,
            author_id: sql.author_id,
            author_name: sql.author_name,
            avatar_url: sql.avatar_url,
            is_guest: sql.is_guest,
            is_redacted: sql.is_redacted,
            author_fingerprint: sql.author_fingerprint,
            content: sql.content,
            created_at: sql.created_at,
            updated_at: sql.updated_at,
            reply_to: sql.reply_to,
            txn_id: sql.txn_id,
        }
    }
}

// 新增：Profile 缓存模型
#[derive(FromRow)]
pub struct SqlProfile {
    pub user_id: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub last_updated_at: NaiveDateTime,
}

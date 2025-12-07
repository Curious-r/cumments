use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Comment {
    pub id: String,
    pub site_id: String,
    pub post_slug: String,
    pub author_id: String,
    pub author_name: String,
    pub is_guest: bool,
    pub is_redacted: bool,
    pub content: String,
    pub created_at: NaiveDateTime,
    pub reply_to: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostCommentCmd {
    pub post_slug: String,
    pub content: String,
    pub nickname: String,
    pub challenge_response: String,
}

#[derive(Debug)]
pub enum MatrixCommand {
    SendComment {
        site_id: String,
        post_slug: String,
        content: String,
    },
}

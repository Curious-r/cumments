use crate::models::SiteId;

#[derive(Debug)]
pub enum AppCommand {
    SendComment {
        site_id: SiteId,
        post_slug: String,
        content: String,
        nickname: String,
        email: Option<String>,
        guest_token: String,
        reply_to: Option<String>,
        txn_id: Option<String>, // 新增：支持幂等去重
    },
    RedactComment {
        site_id: SiteId,
        post_slug: String,
        comment_id: String,
        reason: Option<String>,
    },
    UserDeleteComment {
        site_id: SiteId,
        post_slug: String,
        comment_id: String,
        user_fingerprint: String,
    },
    UserEditComment {
        site_id: SiteId,
        post_slug: String,
        comment_id: String,
        content: String,
        user_fingerprint: String,
    },
}

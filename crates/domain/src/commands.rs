use crate::models::SiteId;

#[derive(Debug)]
pub enum AppCommand {
    SendComment {
        site_id: SiteId,
        post_slug: String,
        content: String,
        nickname: String,
        reply_to: Option<String>,
        email: Option<String>,
        guest_token: String,
    },
}

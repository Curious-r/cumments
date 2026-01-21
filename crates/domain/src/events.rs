use crate::models::{Comment, SiteId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IngestEvent {
    CommentSaved {
        site_id: SiteId,
        post_slug: String,
        comment: Comment,
    },
    CommentDeleted {
        site_id: SiteId,
        post_slug: String,
        comment_id: String,
    },
}

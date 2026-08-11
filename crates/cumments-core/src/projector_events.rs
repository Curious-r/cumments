use crate::models::Comment;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum ProjectorEvent {
    CommentCreated {
        site_id: String,
        post_slug: String,
        comment: Comment,
    },
    CommentUpdated {
        site_id: String,
        post_slug: String,
        comment: Comment,
    },
    CommentDeleted {
        site_id: String,
        post_slug: String,
        event_id: String,
    },
}

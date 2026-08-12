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
        /// Queue row ID of the delete intent, when the deletion was issued
        /// through the Cumments API.
        #[serde(skip_serializing_if = "Option::is_none")]
        intent_id: Option<i64>,
    },
}

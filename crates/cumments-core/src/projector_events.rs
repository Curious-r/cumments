use crate::models::Message;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum ProjectorEvent {
    MessageCreated {
        site_id: String,
        page_slug: String,
        message: Message,
    },
    MessageUpdated {
        site_id: String,
        page_slug: String,
        message: Message,
    },
    /// A message's annotations (reactions, poll responses) changed without
    /// the message content itself being edited.
    MessageAnnotationsChanged {
        site_id: String,
        page_slug: String,
        message: Message,
    },
    MessageDeleted {
        site_id: String,
        page_slug: String,
        event_id: String,
        /// Queue row ID of the delete submission, when the deletion was issued
        /// through the Cumments API.
        #[serde(skip_serializing_if = "Option::is_none")]
        submission_id: Option<i64>,
    },
    /// A new Poll comment was projected.
    PollCreated {
        site_id: String,
        page_slug: String,
        poll_id: String,
        message: Message,
    },
    /// A Poll vote fact caused the effective Poll projection/view to change.
    PollVoted {
        site_id: String,
        page_slug: String,
        poll_id: String,
        #[serde(default)]
        results: Option<serde_json::Map<String, serde_json::Value>>,
        total_votes: u64,
    },
    /// A valid Poll end closed the Poll, exposing final unmasked results.
    PollEnded {
        site_id: String,
        page_slug: String,
        poll_id: String,
        results: serde_json::Map<String, serde_json::Value>,
        total_votes: u64,
        end_time: i64,
    },
}

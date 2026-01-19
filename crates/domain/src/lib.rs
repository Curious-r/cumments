use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::fmt;

// --- Value Objects ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SiteId(String);

impl SiteId {
    pub fn new(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if s.contains('_') {
            return Err("Site ID cannot contain underscores ('_'). Please use hyphens ('-') or dots ('.') instead.".to_string());
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        {
            return Err("Site ID contains invalid characters.".to_string());
        }
        if s.len() > 64 {
            return Err("Site ID is too long (max 64 chars).".to_string());
        }
        Ok(Self(s))
    }

    pub fn new_unchecked(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// --- Entities ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub site_id: SiteId,
    pub post_slug: String,
    pub author_id: String,
    pub author_name: String,
    pub is_guest: bool,
    pub is_redacted: bool,
    pub content: String,
    pub created_at: NaiveDateTime,
    pub reply_to: Option<String>,
    pub updated_at: Option<NaiveDateTime>,
}

// --- Application Commands ---
#[derive(Debug)]
pub enum AppCommand {
    SendComment {
        site_id: SiteId,
        post_slug: String,
        content: String,
        nickname: String,
        reply_to: Option<String>,
    },
}

// --- Protocol V1 Implementation ---
pub mod protocol {
    use super::SiteId;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    #[derive(Serialize, Deserialize, Debug)]
    pub struct CummentsMetadata {
        pub author_name: String,
        pub is_guest: bool,
        pub origin_content: String,
    }

    pub fn parse_room_alias(localpart: &str) -> Option<(SiteId, String)> {
        let localpart = localpart.trim_start_matches('#');
        let (site_id_str, slug) = localpart.split_once('_')?;
        let site_id = SiteId::new(site_id_str).ok()?;
        Some((site_id, slug.to_string()))
    }

    pub fn build_outbound_event(nickname: &str, content: &str) -> Value {
        let body_fallback = format!("**{}** (Guest): {}", nickname, content);
        let metadata = CummentsMetadata {
            author_name: nickname.to_string(),
            is_guest: true,
            origin_content: content.to_string(),
        };

        serde_json::json!({
            "msgtype": "m.text",
            "body": body_fallback,
            "com.cumments.v1": metadata
        })
    }

    pub fn extract_comment_data(
        content_json: &Value,
        sender_id: &str,
        bot_id: &str,
    ) -> (String, bool, String) {
        if let Some(metadata_val) = content_json.get("com.cumments.v1") {
            if let Ok(meta) = serde_json::from_value::<CummentsMetadata>(metadata_val.clone()) {
                return (meta.author_name, meta.is_guest, meta.origin_content);
            }
        }

        let body = content_json
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if sender_id == bot_id {
            let parts: Vec<&str> = body.splitn(2, " (Guest): ").collect();
            if parts.len() == 2 {
                let nick = parts[0]
                    .trim_start_matches("**")
                    .trim_end_matches("**")
                    .to_string();
                return (nick, true, parts[1].to_string());
            }
            return ("Bot".to_string(), false, body.to_string());
        }

        (sender_id.to_string(), false, body.to_string())
    }
}

// --- Events ---
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

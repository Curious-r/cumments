use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::fmt;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub site_id: SiteId,
    pub post_slug: String,
    pub author_id: String,
    pub author_name: String,
    pub is_guest: bool,
    pub is_redacted: bool,
    pub author_fingerprint: Option<String>,
    pub content: String,
    pub created_at: NaiveDateTime,
    pub reply_to: Option<String>,
    pub updated_at: Option<NaiveDateTime>,
}

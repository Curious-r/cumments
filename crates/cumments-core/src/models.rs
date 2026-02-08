//! Defines the core data models of the application.
//! These models should be pure data structures with no logic tied to infrastructure.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// A validated, owned representation of a Site ID.
// More validation logic will be added later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SiteId(String);

impl SiteId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// A validated, owned representation of a Post Slug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PostSlug(String);

impl PostSlug {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A comment that has been projected into our read database.
/// This is the data structure that will be returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Comment {
    pub event_id: String,
    pub author_nickname: Option<String>,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

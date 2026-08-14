//! Audit records for chat-driven management commands.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Outcome of a processed chat command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAuditStatus {
    Ok,
    Denied,
    Invalid,
    RateLimited,
    Error,
}

impl CommandAuditStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Denied => "denied",
            Self::Invalid => "invalid",
            Self::RateLimited => "rate_limited",
            Self::Error => "error",
        }
    }
}

impl FromStr for CommandAuditStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ok" => Ok(Self::Ok),
            "denied" => Ok(Self::Denied),
            "invalid" => Ok(Self::Invalid),
            "rate_limited" => Ok(Self::RateLimited),
            "error" => Ok(Self::Error),
            other => Err(format!("unknown command audit status `{other}`")),
        }
    }
}

/// One audited chat management command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAuditEntry {
    pub id: i64,
    pub actor_mxid: String,
    /// The room the command arrived in (typically a private DM).
    pub room_id: String,
    /// The raw command text.
    pub command: String,
    /// The target site the command resolved to, when applicable.
    pub site_id: Option<String>,
    pub status: CommandAuditStatus,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Payload for recording a command without an id/timestamp yet.
#[derive(Debug, Clone)]
pub struct NewCommandAuditEntry {
    pub actor_mxid: String,
    pub room_id: String,
    pub command: String,
    pub site_id: Option<String>,
    pub status: CommandAuditStatus,
    pub error: Option<String>,
}

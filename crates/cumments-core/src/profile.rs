//! Profile domain models, mutation operations, and lifecycle state machines.
//!
//! Enforces strict separation between visitor identity (Ed25519 public key),
//! site-scoped virtual user profiles, and content publishing.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::media_reference::MediaReference;
use crate::models::SiteId;

/// Error parsing a `ProfileField` from a string.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileFieldError {
    #[error("unknown profile field: '{0}' (expected 'display_name' or 'avatar')")]
    UnknownField(String),
}

/// The profile field targeted by a mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileField {
    DisplayName,
    Avatar,
}

impl ProfileField {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DisplayName => "display_name",
            Self::Avatar => "avatar",
        }
    }
}

impl fmt::Display for ProfileField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProfileField {
    type Err = ProfileFieldError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "display_name" => Ok(Self::DisplayName),
            "avatar" => Ok(Self::Avatar),
            other => Err(ProfileFieldError::UnknownField(other.to_string())),
        }
    }
}

/// Error parsing a `ProfileOperationStatus` from a string.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileOperationStatusError {
    #[error("unknown profile operation status: '{0}'")]
    UnknownStatus(String),
}

/// Lifecycle execution status of a visitor profile mutation.
///
/// Distinct from Matrix send transport execution (`OperationExecutionStatus`).
/// Scoped specifically to the 6-state profile operation lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileOperationStatus {
    /// Accepted, validated, and persisted; awaiting dispatch.
    Pending,
    /// In-flight request currently executing against Matrix Client-Server API.
    Dispatching,
    /// Downstream outcome cannot be proven (timeout, crash, lost response). Non-terminal.
    Unknown,
    /// Proven to have succeeded downstream on Matrix; stores terminal response.
    Completed,
    /// Proven to have failed deterministically downstream (e.g. 4xx validation).
    Failed,
    /// Proven safely not executed, or explicitly abandoned by operational policy.
    Aborted,
}

impl ProfileOperationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatching => "dispatching",
            Self::Unknown => "unknown",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
        }
    }

    /// Whether this operation is still unresolved and must block subsequent mutations
    /// targeting the same visitor and field under strict same-field serialization.
    pub fn is_unresolved(&self) -> bool {
        matches!(self, Self::Pending | Self::Dispatching | Self::Unknown)
    }

    /// Whether this operation has reached a terminal outcome.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Aborted)
    }
}

impl fmt::Display for ProfileOperationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProfileOperationStatus {
    type Err = ProfileOperationStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "dispatching" => Ok(Self::Dispatching),
            "unknown" => Ok(Self::Unknown),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "aborted" => Ok(Self::Aborted),
            other => Err(ProfileOperationStatusError::UnknownStatus(
                other.to_string(),
            )),
        }
    }
}

/// The semantic target value of a profile mutation.
///
/// Explicitly distinguishes between setting a new value and clearing the field.
/// Clearing is an explicit domain action, never represented by an empty string or sentinel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ProfileTargetValue {
    SetDisplayName(String),
    ClearDisplayName,
    SetAvatar(MediaReference),
    ClearAvatar,
}

impl ProfileTargetValue {
    /// Returns the target `ProfileField` associated with this mutation intent.
    pub fn field(&self) -> ProfileField {
        match self {
            Self::SetDisplayName(_) | Self::ClearDisplayName => ProfileField::DisplayName,
            Self::SetAvatar(_) | Self::ClearAvatar => ProfileField::Avatar,
        }
    }

    /// Whether this mutation explicitly clears/unsets the targeted field.
    pub fn is_clear(&self) -> bool {
        matches!(self, Self::ClearDisplayName | Self::ClearAvatar)
    }
}

/// The domain representation of a durable profile mutation operation.
///
/// Represents an authenticated visitor's intent to mutate a single profile field,
/// tracking its serialization sequence and execution lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileOperation {
    /// Logical operation identity (client-supplied `Idempotency-Key` header).
    pub operation_id: String,
    /// Ed25519 public key of the authenticated visitor.
    pub author_public_key: String,
    /// Site scope of the virtual user.
    pub site_id: SiteId,
    /// Target profile field (`display_name` or `avatar`).
    pub field: ProfileField,
    /// Semantic target value (set with value or explicit clear).
    pub target_value: ProfileTargetValue,
    /// Lifecycle execution status.
    pub status: ProfileOperationStatus,
    /// Monotonic per-(author_public_key, field) sequence number for strict serialization.
    pub sequence: i64,
    /// Terminal response payload serialized for idempotent replay, if completed.
    pub response_payload: Option<String>,
    /// Terminal error detail if failed or aborted.
    pub error_detail: Option<String>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
    /// Terminal resolution timestamp (when transitioned to Completed, Failed, or Aborted).
    pub resolved_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_field_display_and_parsing() {
        assert_eq!(ProfileField::DisplayName.as_str(), "display_name");
        assert_eq!(ProfileField::Avatar.as_str(), "avatar");

        assert_eq!(
            "display_name".parse::<ProfileField>().unwrap(),
            ProfileField::DisplayName
        );
        assert_eq!(
            "avatar".parse::<ProfileField>().unwrap(),
            ProfileField::Avatar
        );
        assert!("unknown".parse::<ProfileField>().is_err());

        assert_eq!(ProfileField::DisplayName.to_string(), "display_name");
        assert_eq!(ProfileField::Avatar.to_string(), "avatar");
    }

    #[test]
    fn profile_field_serde() {
        let serialized = serde_json::to_string(&ProfileField::DisplayName).unwrap();
        assert_eq!(serialized, "\"display_name\"");
        let deserialized: ProfileField = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, ProfileField::DisplayName);
    }

    #[test]
    fn profile_operation_status_lifecycle() {
        assert!(ProfileOperationStatus::Pending.is_unresolved());
        assert!(ProfileOperationStatus::Dispatching.is_unresolved());
        assert!(ProfileOperationStatus::Unknown.is_unresolved());
        assert!(!ProfileOperationStatus::Completed.is_unresolved());
        assert!(!ProfileOperationStatus::Failed.is_unresolved());
        assert!(!ProfileOperationStatus::Aborted.is_unresolved());

        assert!(!ProfileOperationStatus::Pending.is_terminal());
        assert!(!ProfileOperationStatus::Dispatching.is_terminal());
        assert!(!ProfileOperationStatus::Unknown.is_terminal());
        assert!(ProfileOperationStatus::Completed.is_terminal());
        assert!(ProfileOperationStatus::Failed.is_terminal());
        assert!(ProfileOperationStatus::Aborted.is_terminal());
    }

    #[test]
    fn profile_operation_status_parsing_and_serde() {
        for (str_val, status) in [
            ("pending", ProfileOperationStatus::Pending),
            ("dispatching", ProfileOperationStatus::Dispatching),
            ("unknown", ProfileOperationStatus::Unknown),
            ("completed", ProfileOperationStatus::Completed),
            ("failed", ProfileOperationStatus::Failed),
            ("aborted", ProfileOperationStatus::Aborted),
        ] {
            assert_eq!(status.as_str(), str_val);
            assert_eq!(status.to_string(), str_val);
            assert_eq!(str_val.parse::<ProfileOperationStatus>().unwrap(), status);

            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{str_val}\""));
            let parsed: ProfileOperationStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
        assert!("invalid".parse::<ProfileOperationStatus>().is_err());
    }

    #[test]
    fn profile_target_value_semantics() {
        let set_name = ProfileTargetValue::SetDisplayName("Alice".to_string());
        assert_eq!(set_name.field(), ProfileField::DisplayName);
        assert!(!set_name.is_clear());

        let clear_name = ProfileTargetValue::ClearDisplayName;
        assert_eq!(clear_name.field(), ProfileField::DisplayName);
        assert!(clear_name.is_clear());

        let media_ref = MediaReference::new_v4();
        let set_avatar = ProfileTargetValue::SetAvatar(media_ref.clone());
        assert_eq!(set_avatar.field(), ProfileField::Avatar);
        assert!(!set_avatar.is_clear());

        let clear_avatar = ProfileTargetValue::ClearAvatar;
        assert_eq!(clear_avatar.field(), ProfileField::Avatar);
        assert!(clear_avatar.is_clear());

        // Serde roundtrip
        for target in [set_name, clear_name, set_avatar, clear_avatar] {
            let json = serde_json::to_string(&target).unwrap();
            let deserialized: ProfileTargetValue = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, target);
        }
    }
}

//! Profile domain models, mutation operations, and lifecycle state machines.
//!
//! Enforces strict separation between visitor identity (Ed25519 public key),
//! site-scoped virtual user profiles, and content publishing.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::canonical::CanonicalJson;
use crate::media_reference::MediaReference;
use crate::models::SiteId;
use crate::ports::{MatrixProfileDriver, MediaReferenceResolver, ProfileStore};

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

    /// Converts this mutation into its canonical JSON version-1 representation.
    pub fn to_canonical_json(&self, site_id: &str) -> CanonicalJson {
        match self {
            Self::SetDisplayName(name) => CanonicalJson::array(vec![
                CanonicalJson::string("SET_DISPLAY_NAME"),
                CanonicalJson::string(site_id),
                CanonicalJson::string(name),
            ]),
            Self::ClearDisplayName => CanonicalJson::array(vec![
                CanonicalJson::string("CLEAR_DISPLAY_NAME"),
                CanonicalJson::string(site_id),
            ]),
            Self::SetAvatar(media_ref) => CanonicalJson::array(vec![
                CanonicalJson::string("SET_AVATAR"),
                CanonicalJson::string(site_id),
                CanonicalJson::string(media_ref.as_str()),
            ]),
            Self::ClearAvatar => CanonicalJson::array(vec![
                CanonicalJson::string("CLEAR_AVATAR"),
                CanonicalJson::string(site_id),
            ]),
        }
    }

    /// Computes the hex-encoded SHA-256 semantic fingerprint over the canonical JSON bytes.
    pub fn semantic_fingerprint(&self, site_id: &str) -> String {
        crate::site_auth::sha256_hex(&self.to_canonical_json(site_id).to_canonical_bytes())
    }

    /// Serializes this semantic value for SQLite storage (NULL for explicit clear).
    pub fn to_stored(&self) -> Option<String> {
        match self {
            Self::ClearDisplayName | Self::ClearAvatar => None,
            Self::SetDisplayName(name) => Some(name.clone()),
            Self::SetAvatar(media_ref) => Some(media_ref.to_string()),
        }
    }

    /// Deserializes a stored SQLite value (or NULL for clear) given the target field.
    pub fn from_stored(field: ProfileField, stored: Option<&str>) -> Result<Self, String> {
        match (field, stored) {
            (ProfileField::DisplayName, None) => Ok(Self::ClearDisplayName),
            (ProfileField::Avatar, None) => Ok(Self::ClearAvatar),
            (ProfileField::DisplayName, Some(s)) => {
                if s.starts_with('{')
                    && let Ok(val) = serde_json::from_str::<Self>(s)
                {
                    return Ok(val);
                }
                Ok(Self::SetDisplayName(s.to_string()))
            }
            (ProfileField::Avatar, Some(s)) => {
                if s.starts_with('{')
                    && let Ok(val) = serde_json::from_str::<Self>(s)
                {
                    return Ok(val);
                }
                let media_ref = MediaReference::parse(s)
                    .map_err(|e| format!("invalid media reference in stored profile op: {e}"))?;
                Ok(Self::SetAvatar(media_ref))
            }
        }
    }
}

/// Builds the canonical signature envelope signed for a visitor profile mutation.
///
/// Format: `["OP_NAME", site_id, operation_id, semantic_fingerprint]`
pub fn profile_signature_message(
    target_value: &ProfileTargetValue,
    site_id: &str,
    operation_id: &str,
) -> String {
    let op_name = match target_value {
        ProfileTargetValue::SetDisplayName(_) => "SET_DISPLAY_NAME",
        ProfileTargetValue::ClearDisplayName => "CLEAR_DISPLAY_NAME",
        ProfileTargetValue::SetAvatar(_) => "SET_AVATAR",
        ProfileTargetValue::ClearAvatar => "CLEAR_AVATAR",
    };
    let fingerprint = target_value.semantic_fingerprint(site_id);
    crate::identity::signature_message(&[
        Some(op_name),
        Some(site_id),
        Some(operation_id),
        Some(&fingerprint),
    ])
}

/// Verifies an Ed25519 signature over the canonical profile mutation envelope.
pub fn verify_profile_signature(
    public_key_b64: &str,
    target_value: &ProfileTargetValue,
    site_id: &str,
    operation_id: &str,
    signature_b64: &str,
) -> bool {
    let message = profile_signature_message(target_value, site_id, operation_id);
    crate::identity::verify_signature(public_key_b64, &message, signature_b64)
}

pub fn set_display_name_signature_message(
    site_id: &str,
    operation_id: &str,
    display_name: &str,
) -> String {
    profile_signature_message(
        &ProfileTargetValue::SetDisplayName(display_name.to_string()),
        site_id,
        operation_id,
    )
}

pub fn clear_display_name_signature_message(site_id: &str, operation_id: &str) -> String {
    profile_signature_message(&ProfileTargetValue::ClearDisplayName, site_id, operation_id)
}

pub fn set_avatar_signature_message(
    site_id: &str,
    operation_id: &str,
    media_ref: &MediaReference,
) -> String {
    profile_signature_message(
        &ProfileTargetValue::SetAvatar(media_ref.clone()),
        site_id,
        operation_id,
    )
}

pub fn clear_avatar_signature_message(site_id: &str, operation_id: &str) -> String {
    profile_signature_message(&ProfileTargetValue::ClearAvatar, site_id, operation_id)
}

/// The outcome of an atomic operation claim attempt for a profile mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileClaimOutcome {
    /// Newly claimed operation; persisted in `Pending` status with allocated sequence number.
    New(ProfileOperation),
    /// Idempotent replay of an existing operation with identical author and semantic intent.
    Replay(ProfileOperation),
    /// Conflicting reuse of `operation_id` with different author, site, or semantic fingerprint.
    Conflict,
}

/// Profile mutation errors categorized strictly by determinism for safe state transitions.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ProfileDriverError {
    /// Deterministic failure downstream (HTTP 4xx validation or client error).
    /// Safely transitions to `Failed`.
    #[error("deterministic downstream failure: {0}")]
    Deterministic(String),

    /// Ambiguous outcome downstream (HTTP 5xx, network timeout, connection lost).
    /// Must transition to `Unknown` to prevent split-brain and stale overwrites.
    #[error("ambiguous downstream outcome: {0}")]
    Ambiguous(String),
}

/// Result of an attempt to execute a profile mutation operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileOperationExecutionResult {
    /// Operation successfully dispatched and confirmed downstream on Matrix.
    Completed,
    /// Operation deterministically failed downstream on Matrix.
    Failed(String),
    /// Operation dispatch outcome is ambiguous (timeout, connection drop).
    Unknown(String),
    /// Operation is blocked from execution because an earlier operation for this
    /// visitor + field is still unresolved (`Pending`, `Dispatching`, `Unknown`).
    Blocked(String),
    /// Operation was already processed to a terminal state (`Completed`, `Failed`, `Aborted`).
    AlreadyProcessed(ProfileOperation),
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

/// Durable executor for visitor profile mutations.
///
/// Strictly enforces same-field serialization without in-memory locks, coordinates
/// with Matrix homeserver Client-Server profile APIs, and records terminal or
/// ambiguous outcomes according to frozen architecture invariants.
pub struct ProfileOperationExecutor {
    store: Arc<dyn ProfileStore>,
    driver: Arc<dyn MatrixProfileDriver>,
    media_resolver: Option<Arc<dyn MediaReferenceResolver>>,
}

impl ProfileOperationExecutor {
    pub fn new(
        store: Arc<dyn ProfileStore>,
        driver: Arc<dyn MatrixProfileDriver>,
        media_resolver: Option<Arc<dyn MediaReferenceResolver>>,
    ) -> Self {
        Self {
            store,
            driver,
            media_resolver,
        }
    }

    /// Attempts to execute the specified operation by ID.
    ///
    /// Respects same-field serialization invariants:
    /// - If earlier unresolved operations exist for this (visitor, field), execution is blocked.
    /// - If currently in `Dispatching` or `Unknown`, execution is blocked.
    /// - If terminal, returns `AlreadyProcessed`.
    /// - If successfully claimed, transitions to `Dispatching`, calls driver, and transitions to
    ///   `Completed`, `Failed`, or `Unknown`.
    pub async fn execute(
        &self,
        operation_id: &str,
    ) -> anyhow::Result<ProfileOperationExecutionResult> {
        let op = match self.store.get_profile_operation(operation_id).await? {
            Some(op) => op,
            None => anyhow::bail!("profile operation '{operation_id}' not found"),
        };

        if op.status.is_terminal() {
            return Ok(ProfileOperationExecutionResult::AlreadyProcessed(op));
        }

        if op.status == ProfileOperationStatus::Unknown {
            return Ok(ProfileOperationExecutionResult::Blocked(
                "operation is in unknown status and cannot be re-executed".to_string(),
            ));
        }

        if op.status == ProfileOperationStatus::Dispatching {
            return Ok(ProfileOperationExecutionResult::Blocked(
                "operation is currently dispatching".to_string(),
            ));
        }

        let claimed = self.store.claim_for_execution(operation_id).await?;
        if !claimed {
            return Ok(ProfileOperationExecutionResult::Blocked(
                "operation is blocked by an earlier unresolved operation for this field"
                    .to_string(),
            ));
        }

        // Lease acquired, operation is in Dispatching. Dispatch to Matrix driver.
        let driver_res = match &op.target_value {
            ProfileTargetValue::SetDisplayName(name) => {
                self.driver
                    .set_display_name(&op.author_public_key, &op.site_id, name)
                    .await
            }
            ProfileTargetValue::ClearDisplayName => {
                self.driver
                    .clear_display_name(&op.author_public_key, &op.site_id)
                    .await
            }
            ProfileTargetValue::SetAvatar(media_ref) => {
                let resolved_mxc = match &self.media_resolver {
                    Some(resolver) => resolver.resolve_mxc(&op.site_id, media_ref).await,
                    None => Ok(None),
                };
                let mxc_url = match resolved_mxc {
                    Ok(Some(mxc_url)) => mxc_url,
                    Ok(None) => {
                        let err = format!(
                            "unresolvable media reference '{media_ref}' for site '{}'",
                            op.site_id.as_str()
                        );
                        self.store.record_failed(operation_id, &err).await?;
                        return Ok(ProfileOperationExecutionResult::Failed(err));
                    }
                    Err(e) => {
                        let err = format!("failed to resolve media reference '{media_ref}': {e}");
                        self.store.record_unknown(operation_id, &err).await?;
                        return Ok(ProfileOperationExecutionResult::Unknown(err));
                    }
                };
                self.driver
                    .set_avatar(&op.author_public_key, &op.site_id, &mxc_url)
                    .await
            }
            ProfileTargetValue::ClearAvatar => {
                self.driver
                    .clear_avatar(&op.author_public_key, &op.site_id)
                    .await
            }
        };

        match driver_res {
            Ok(()) => {
                self.store.record_completed(operation_id, None).await?;
                Ok(ProfileOperationExecutionResult::Completed)
            }
            Err(ProfileDriverError::Deterministic(err)) => {
                self.store.record_failed(operation_id, &err).await?;
                Ok(ProfileOperationExecutionResult::Failed(err))
            }
            Err(ProfileDriverError::Ambiguous(err)) => {
                self.store.record_unknown(operation_id, &err).await?;
                Ok(ProfileOperationExecutionResult::Unknown(err))
            }
        }
    }

    /// Attempts to execute the next pending operation for the given visitor and field.
    pub async fn execute_next(
        &self,
        author_public_key: &str,
        field: ProfileField,
    ) -> anyhow::Result<Option<ProfileOperationExecutionResult>> {
        let next_op = self
            .store
            .get_next_executable_operation(author_public_key, field)
            .await?;

        match next_op {
            Some(op) => {
                let res = self.execute(&op.operation_id).await?;
                Ok(Some(res))
            }
            None => Ok(None),
        }
    }
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

    #[test]
    fn profile_target_value_canonical_and_fingerprint() {
        let name_alice = ProfileTargetValue::SetDisplayName("Alice".to_string());
        let name_bob = ProfileTargetValue::SetDisplayName("Bob".to_string());
        let clear_name = ProfileTargetValue::ClearDisplayName;

        let fp_alice = name_alice.semantic_fingerprint("blog");
        let fp_alice_again = name_alice.semantic_fingerprint("blog");
        let fp_alice_other_site = name_alice.semantic_fingerprint("other-blog");
        let fp_bob = name_bob.semantic_fingerprint("blog");
        let fp_clear = clear_name.semantic_fingerprint("blog");

        assert_eq!(fp_alice, fp_alice_again);
        assert_ne!(fp_alice, fp_alice_other_site);
        assert_ne!(fp_alice, fp_bob);
        assert_ne!(fp_alice, fp_clear);

        let media1 = MediaReference::new_v4();
        let media2 = MediaReference::new_v4();
        let avatar1 = ProfileTargetValue::SetAvatar(media1);
        let avatar2 = ProfileTargetValue::SetAvatar(media2);
        let clear_avatar = ProfileTargetValue::ClearAvatar;

        let fp_av1 = avatar1.semantic_fingerprint("blog");
        let fp_av2 = avatar2.semantic_fingerprint("blog");
        let fp_clear_av = clear_avatar.semantic_fingerprint("blog");

        assert_ne!(fp_av1, fp_av2);
        assert_ne!(fp_av1, fp_clear_av);
    }

    #[test]
    fn profile_target_value_stored_roundtrip() {
        let name_val = ProfileTargetValue::SetDisplayName("Alice".to_string());
        let stored_name = name_val.to_stored();
        assert_eq!(stored_name.as_deref(), Some("Alice"));
        let recovered_name =
            ProfileTargetValue::from_stored(ProfileField::DisplayName, stored_name.as_deref())
                .unwrap();
        assert_eq!(recovered_name, name_val);

        let clear_name = ProfileTargetValue::ClearDisplayName;
        assert_eq!(clear_name.to_stored(), None);
        let recovered_clear_name =
            ProfileTargetValue::from_stored(ProfileField::DisplayName, None).unwrap();
        assert_eq!(recovered_clear_name, clear_name);

        let media = MediaReference::new_v4();
        let avatar_val = ProfileTargetValue::SetAvatar(media.clone());
        let stored_avatar = avatar_val.to_stored();
        assert_eq!(stored_avatar.as_deref(), Some(media.as_str()));
        let recovered_avatar =
            ProfileTargetValue::from_stored(ProfileField::Avatar, stored_avatar.as_deref())
                .unwrap();
        assert_eq!(recovered_avatar, avatar_val);

        let clear_avatar = ProfileTargetValue::ClearAvatar;
        assert_eq!(clear_avatar.to_stored(), None);
        let recovered_clear_avatar =
            ProfileTargetValue::from_stored(ProfileField::Avatar, None).unwrap();
        assert_eq!(recovered_clear_avatar, clear_avatar);
    }

    #[test]
    fn profile_driver_error_display() {
        let det = ProfileDriverError::Deterministic("400 Bad Request".to_string());
        assert!(det.to_string().contains("deterministic"));

        let amb = ProfileDriverError::Ambiguous("504 Gateway Timeout".to_string());
        assert!(amb.to_string().contains("ambiguous"));
    }

    #[test]
    fn profile_signature_canonicalization_and_verification() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

        let site = "my-site";
        let op_id = "op-uuid-123";
        let chal = "chal-pow-abc";

        let target_set_name = ProfileTargetValue::SetDisplayName("Alice".to_string());
        let target_clear_name = ProfileTargetValue::ClearDisplayName;
        let media_ref = MediaReference::new_v4();
        let target_set_avatar = ProfileTargetValue::SetAvatar(media_ref.clone());
        let target_clear_avatar = ProfileTargetValue::ClearAvatar;

        // 1. Signature messages follow the canonical envelope structure
        let msg_set_name = set_display_name_signature_message(site, op_id, "Alice");
        let expected_fp = target_set_name.semantic_fingerprint(site);
        assert_eq!(
            msg_set_name,
            format!(
                "[\"SET_DISPLAY_NAME\",\"{}\",\"{}\",\"{}\"]",
                site, op_id, expected_fp
            )
        );

        let msg_clear_name = clear_display_name_signature_message(site, op_id);
        let expected_clear_fp = target_clear_name.semantic_fingerprint(site);
        assert_eq!(
            msg_clear_name,
            format!(
                "[\"CLEAR_DISPLAY_NAME\",\"{}\",\"{}\",\"{}\"]",
                site, op_id, expected_clear_fp
            )
        );

        let msg_set_avatar = set_avatar_signature_message(site, op_id, &media_ref);
        let expected_av_fp = target_set_avatar.semantic_fingerprint(site);
        assert_eq!(
            msg_set_avatar,
            format!(
                "[\"SET_AVATAR\",\"{}\",\"{}\",\"{}\"]",
                site, op_id, expected_av_fp
            )
        );

        let msg_clear_avatar = clear_avatar_signature_message(site, op_id);
        let expected_clear_av_fp = target_clear_avatar.semantic_fingerprint(site);
        assert_eq!(
            msg_clear_avatar,
            format!(
                "[\"CLEAR_AVATAR\",\"{}\",\"{}\",\"{}\"]",
                site, op_id, expected_clear_av_fp
            )
        );

        // 2. Sign and verify valid signatures
        let sig_set_name =
            URL_SAFE_NO_PAD.encode(signing_key.sign(msg_set_name.as_bytes()).to_bytes());
        assert!(verify_profile_signature(
            &public_key,
            &target_set_name,
            site,
            op_id,
            &sig_set_name
        ));

        // 3. Signature for SetDisplayName("Alice") fails if verified against SetDisplayName("Bob")
        let target_set_bob = ProfileTargetValue::SetDisplayName("Bob".to_string());
        assert!(!verify_profile_signature(
            &public_key,
            &target_set_bob,
            site,
            op_id,
            &sig_set_name
        ));

        // 4. Signature for SetDisplayName fails on ClearDisplayName
        assert!(!verify_profile_signature(
            &public_key,
            &target_clear_name,
            site,
            op_id,
            &sig_set_name
        ));

        // 5. Signature bound to site: fails on another site
        assert!(!verify_profile_signature(
            &public_key,
            &target_set_name,
            "other-site",
            op_id,
            &sig_set_name
        ));

        // 6. Signature bound to operation_id: fails on different operation_id
        assert!(!verify_profile_signature(
            &public_key,
            &target_set_name,
            site,
            "other-op-456",
            &sig_set_name
        ));

        // 7. Signature does NOT contain PoW challenge, so it authorizes the semantic operation
        // independent of whatever PoW challenge is verified at HTTP admission.
        let other_chal = "some-other-challenge";
        assert_ne!(chal, other_chal);
        assert!(verify_profile_signature(
            &public_key,
            &target_set_name,
            site,
            op_id,
            &sig_set_name
        ));
    }
}

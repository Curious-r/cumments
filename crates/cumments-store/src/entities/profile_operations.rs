use super::active_enums::{ProfileField, ProfileOperationStatus};
use sea_orm::entity::prelude::*;

/// Durable profile mutation state machine and strict same-field serialization.
///
/// Keeps profile mutation lifecycle and serialization sequence separate from
/// Matrix send execution (`operation_executions`).
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "profile_operations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Client-allocated logical operation identity (HTTP `Idempotency-Key`).
    #[sea_orm(unique)]
    pub operation_id: String,
    /// Authenticated visitor Ed25519 public key.
    pub author_public_key: String,
    /// Site scope of the virtual user.
    pub site_id: String,
    /// Target profile field (`display_name` or `avatar`).
    pub field: ProfileField,
    /// Semantic target value (serialized JSON or string representation, NULL for clear).
    pub target_value: Option<String>,
    /// Lifecycle execution status (`pending`, `dispatching`, `unknown`, `completed`, `failed`, `aborted`).
    pub status: ProfileOperationStatus,
    /// Monotonic sequence number per (site_id, author_public_key, field).
    pub sequence: i64,
    /// Terminal response payload serialized for idempotent replay, if completed.
    pub response_payload: Option<String>,
    /// Terminal error detail if failed or aborted.
    pub error_detail: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub resolved_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

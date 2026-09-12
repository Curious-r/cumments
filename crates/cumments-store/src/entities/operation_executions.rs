use super::active_enums::OperationExecutionStatus;
use sea_orm::entity::prelude::*;

/// Transport execution state for a claimed operation.
///
/// Keeps the Matrix `txn_id` and delivery status separate from the
/// logical [`operation_claims`] identity. Even if an operation does not
/// create a `post_submissions` row (e.g. Vote), it persists its downstream
/// transaction ID here before sending to Matrix so retries reuse the exact
/// same `txn_id`.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "operation_executions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Server-wide logical operation identity (HTTP `Idempotency-Key`).
    #[sea_orm(unique)]
    pub operation_id: String,
    /// Downstream Matrix transaction ID used for the send request.
    pub txn_id: String,
    /// Transport lifecycle status (`in_flight`, `success`, `failed`).
    pub status: OperationExecutionStatus,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

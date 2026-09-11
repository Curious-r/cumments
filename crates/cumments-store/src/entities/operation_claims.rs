use sea_orm::entity::prelude::*;

/// The server-wide claim of one logical mutation operation.
///
/// `operation_id` (the HTTP `Idempotency-Key`) is unique across the whole
/// server, not scoped to the author: this table is the database-level
/// enforcement of the frozen invariant "one `operation_id` denotes exactly one
/// logical operation" (frozen Poll design §5.1). It is intentionally separate
/// from `idempotency_keys`, whose uniqueness remains author-scoped for
/// existing comment behavior.
///
/// `submission_id` references the durable submission that carries the Matrix
/// execution state for the claimed operation; it is written in the same
/// transaction as the claim so a claimed operation always has durable work.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "operation_claims")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Server-wide logical operation identity (HTTP `Idempotency-Key`).
    #[sea_orm(unique)]
    pub operation_id: String,
    /// Authenticated author that claimed the operation.
    pub author_public_key: String,
    /// Digest of the canonical semantic operation.
    pub fingerprint: String,
    /// Durable submission created for this operation.
    pub submission_id: i64,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

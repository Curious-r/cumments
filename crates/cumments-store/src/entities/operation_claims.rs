use sea_orm::entity::prelude::*;

/// The server-wide claim of one logical mutation operation.
///
/// `operation_id` (the HTTP `Idempotency-Key`) is unique across the whole
/// server, not scoped to the author: this table is the database-level
/// enforcement of the frozen invariant "one `operation_id` denotes exactly one
/// logical operation" (frozen Poll design §5.1).
///
/// A claim is independent of any durable submission: Create Poll creates a
/// post submission alongside its claim, while Vote/End will claim an operation
/// without any submission at all. The durable submission references its
/// operation (see `post_submissions.operation_id`), never the reverse.
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
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

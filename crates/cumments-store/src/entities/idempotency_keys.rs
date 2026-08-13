use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "idempotency_keys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Author public key that scoped the idempotency key.
    pub author_public_key: String,
    /// Client-supplied `Idempotency-Key` header value.
    pub idempotency_key: String,
    /// Fingerprint of the accepted request (`METHOD\npath\nsha256(body)`).
    pub request_fingerprint: String,
    /// Queue row ID of the submission the key is bound to.
    pub submission_id: i64,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

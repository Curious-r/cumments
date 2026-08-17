use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "media_upload_idempotency")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Visitor public key that owns the upload.
    pub author_public_key: String,
    /// Client-supplied `Idempotency-Key` header value.
    pub idempotency_key: String,
    /// Canonical fingerprint of the accepted upload request.
    pub request_fingerprint: String,
    /// MXC URI returned by the first accepted upload.
    pub mxc_url: String,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

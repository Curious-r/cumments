use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "verification_tokens")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub site_id: String,
    pub origin: String,
    pub token_hash: String,
    /// JSON array of `VerificationMethod` strings, in attempt order.
    pub methods: String,
    pub expires_at: DateTimeUtc,
    pub consumed_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    /// Number of confirm attempts made against this token.
    pub attempts: i64,
}

impl ActiveModelBehavior for ActiveModel {}

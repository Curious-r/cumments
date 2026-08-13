use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sites")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub matrix_space_id: String,
    pub display_name: Option<String>,
    /// How this site authenticates write requests.
    pub auth_mode: super::active_enums::SiteAuthMode,
    /// Whether the site has verified at least one origin.
    pub verification_status: super::active_enums::SiteVerificationStatus,
    /// SHA-256 hash of the claim token that proves ownership of this site.
    pub claim_token_hash: Option<String>,
    /// Whether the site id was caller-chosen at registration. Chosen ids
    /// require origin verification before writes in `optional` mode.
    pub is_custom_id: bool,
    /// HMAC key used to verify site requests, when the site uses secret auth.
    pub secret: Option<String>,
    /// When the site was last verified.
    pub verified_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

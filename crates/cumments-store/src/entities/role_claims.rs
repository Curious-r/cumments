use sea_orm::entity::prelude::*;

/// Token-DM role claims: short-lived process state between role registration
/// and the target MXID proving ownership. Matrix power levels remain the
/// authoritative source once a claim reaches `applied`.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "role_claims")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub site_id: String,
    /// Empty string for site-level roles; comment room ID for moderators.
    pub room_id: String,
    /// The DM room the bot joined to verify this claim, if any.
    pub dm_room_id: Option<String>,
    pub user_id: String,
    pub level: i64,
    pub token_hash: String,
    /// One of `pending` / `activated` / `applied` / `revoked`.
    pub status: String,
    pub expires_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
    pub activated_at: Option<DateTimeUtc>,
    pub applied_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

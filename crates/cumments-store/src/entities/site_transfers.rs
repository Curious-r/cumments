use sea_orm::entity::prelude::*;

/// Site ownership transfers: short-lived process state between the owner
/// starting a handover and the target MXID verifying the pending claim.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "site_transfers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub site_id: String,
    pub target_mxid: String,
    /// One of `pending` / `completed` / `expired` / `cancelled`.
    pub status: String,
    pub expires_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
    pub completed_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

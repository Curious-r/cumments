use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "room_registry")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub room_id: String,
    pub site_id: String,
    pub post_slug: String,
    /// One of `active`, `quarantined`, `superseded`.
    pub status: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// Why the room is quarantined from adoption (e.g. governance check
    /// failed). `None` when the room is not quarantined.
    pub quarantine_reason: Option<String>,
    /// When the room first entered quarantine.
    pub quarantined_at: Option<DateTimeUtc>,
    /// Consecutive adoption failures while quarantined.
    pub adoption_failures: u32,
    /// Next scheduled automatic adoption attempt; `None` means manual
    /// attention is required.
    pub next_attempt_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

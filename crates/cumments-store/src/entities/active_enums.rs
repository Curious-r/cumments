use sea_orm::entity::prelude::*;

/// Lifecycle status of an intent-queue row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum IntentStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "waiting_for_sync")]
    WaitingForSync,
    #[sea_orm(string_value = "failed")]
    Failed,
    #[sea_orm(string_value = "completed")]
    Completed,
}

/// How a site authenticates write requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum SiteAuthMode {
    #[sea_orm(string_value = "origin")]
    Origin,
    #[sea_orm(string_value = "secret")]
    Secret,
}

/// Verification state of an API-registered site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum SiteVerificationStatus {
    #[sea_orm(string_value = "unverified")]
    Unverified,
    #[sea_orm(string_value = "verified")]
    Verified,
}

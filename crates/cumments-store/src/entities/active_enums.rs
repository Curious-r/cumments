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

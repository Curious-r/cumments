use sea_orm::entity::prelude::*;

use cumments_core::models::ProjectionRepairStatus;

/// A locally unprojectable Matrix fact. The payload is intentionally absent:
/// repair fetches the homeserver's authoritative, already-redacted event.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "projection_repairs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub target_event_id: String,
    #[sea_orm(indexed)]
    pub room_id: String,
    pub redaction_event_id: String,
    pub reason: String,
    pub observed_room_version: Option<String>,
    pub status: String,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub next_retry_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub resolved_at: Option<DateTimeUtc>,
}

impl Model {
    pub fn status(&self) -> Result<ProjectionRepairStatus, sea_orm::DbErr> {
        match self.status.as_str() {
            "pending" => Ok(ProjectionRepairStatus::Pending),
            "manual" => Ok(ProjectionRepairStatus::Manual),
            "resolved" => Ok(ProjectionRepairStatus::Resolved),
            other => Err(sea_orm::DbErr::Custom(format!(
                "unknown projection repair status {other}"
            ))),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}

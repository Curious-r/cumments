use sea_orm::entity::prelude::*;

/// Lifecycle status of an submission-queue row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum SubmissionStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "processing")]
    Processing,
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

/// Retirement lifecycle of an API-registered site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum SiteLifecycleStatus {
    #[sea_orm(string_value = "active")]
    Active,
    #[sea_orm(string_value = "retiring")]
    Retiring,
    #[sea_orm(string_value = "retired")]
    Retired,
}

/// Lifecycle status of an operation's transport execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum OperationExecutionStatus {
    #[sea_orm(string_value = "in_flight")]
    InFlight,
    #[sea_orm(string_value = "success")]
    Success,
    #[sea_orm(string_value = "failed")]
    Failed,
}

impl From<cumments_core::submissions::OperationExecutionStatus> for OperationExecutionStatus {
    fn from(status: cumments_core::submissions::OperationExecutionStatus) -> Self {
        match status {
            cumments_core::submissions::OperationExecutionStatus::InFlight => Self::InFlight,
            cumments_core::submissions::OperationExecutionStatus::Success => Self::Success,
            cumments_core::submissions::OperationExecutionStatus::Failed => Self::Failed,
        }
    }
}

impl From<OperationExecutionStatus> for cumments_core::submissions::OperationExecutionStatus {
    fn from(status: OperationExecutionStatus) -> Self {
        match status {
            OperationExecutionStatus::InFlight => Self::InFlight,
            OperationExecutionStatus::Success => Self::Success,
            OperationExecutionStatus::Failed => Self::Failed,
        }
    }
}

/// The profile field targeted by a mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum ProfileField {
    #[sea_orm(string_value = "display_name")]
    DisplayName,
    #[sea_orm(string_value = "avatar")]
    Avatar,
}

impl From<cumments_core::profile::ProfileField> for ProfileField {
    fn from(field: cumments_core::profile::ProfileField) -> Self {
        match field {
            cumments_core::profile::ProfileField::DisplayName => Self::DisplayName,
            cumments_core::profile::ProfileField::Avatar => Self::Avatar,
        }
    }
}

impl From<ProfileField> for cumments_core::profile::ProfileField {
    fn from(field: ProfileField) -> Self {
        match field {
            ProfileField::DisplayName => Self::DisplayName,
            ProfileField::Avatar => Self::Avatar,
        }
    }
}

/// Lifecycle status of a visitor profile mutation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum ProfileOperationStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "dispatching")]
    Dispatching,
    #[sea_orm(string_value = "unknown")]
    Unknown,
    #[sea_orm(string_value = "completed")]
    Completed,
    #[sea_orm(string_value = "failed")]
    Failed,
    #[sea_orm(string_value = "aborted")]
    Aborted,
}

impl From<cumments_core::profile::ProfileOperationStatus> for ProfileOperationStatus {
    fn from(status: cumments_core::profile::ProfileOperationStatus) -> Self {
        match status {
            cumments_core::profile::ProfileOperationStatus::Pending => Self::Pending,
            cumments_core::profile::ProfileOperationStatus::Dispatching => Self::Dispatching,
            cumments_core::profile::ProfileOperationStatus::Unknown => Self::Unknown,
            cumments_core::profile::ProfileOperationStatus::Completed => Self::Completed,
            cumments_core::profile::ProfileOperationStatus::Failed => Self::Failed,
            cumments_core::profile::ProfileOperationStatus::Aborted => Self::Aborted,
        }
    }
}

impl From<ProfileOperationStatus> for cumments_core::profile::ProfileOperationStatus {
    fn from(status: ProfileOperationStatus) -> Self {
        match status {
            ProfileOperationStatus::Pending => Self::Pending,
            ProfileOperationStatus::Dispatching => Self::Dispatching,
            ProfileOperationStatus::Unknown => Self::Unknown,
            ProfileOperationStatus::Completed => Self::Completed,
            ProfileOperationStatus::Failed => Self::Failed,
            ProfileOperationStatus::Aborted => Self::Aborted,
        }
    }
}

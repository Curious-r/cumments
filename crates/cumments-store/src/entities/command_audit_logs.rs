use sea_orm::entity::prelude::*;

/// Audit log of chat-driven management commands.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "command_audit_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub actor_mxid: String,
    pub room_id: String,
    pub command: String,
    pub site_id: Option<String>,
    /// One of `ok` / `denied` / `invalid` / `rate_limited` / `error`.
    pub status: String,
    pub error: Option<String>,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}

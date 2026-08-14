use super::DbStore;
use crate::entities::command_audit_logs;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::audit::{CommandAuditEntry, NewCommandAuditEntry};
use cumments_core::ports::CommandAuditStore;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};

fn to_entry(model: command_audit_logs::Model) -> Result<CommandAuditEntry> {
    Ok(CommandAuditEntry {
        id: model.id,
        actor_mxid: model.actor_mxid,
        room_id: model.room_id,
        command: model.command,
        site_id: model.site_id,
        status: model
            .status
            .parse()
            .map_err(|e: String| anyhow::anyhow!(e))?,
        error: model.error,
        created_at: model.created_at,
    })
}

#[async_trait]
impl CommandAuditStore for DbStore {
    async fn record_command_audit(&self, entry: &NewCommandAuditEntry) -> Result<()> {
        command_audit_logs::Entity::insert(command_audit_logs::ActiveModel {
            actor_mxid: Set(entry.actor_mxid.clone()),
            room_id: Set(entry.room_id.clone()),
            command: Set(entry.command.clone()),
            site_id: Set(entry.site_id.clone()),
            status: Set(entry.status.as_str().to_string()),
            error: Set(entry.error.clone()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        })
        .exec(&self.db)
        .await?;
        Ok(())
    }

    async fn list_command_audit(
        &self,
        actor_mxid: Option<&str>,
        limit: u64,
    ) -> Result<Vec<CommandAuditEntry>> {
        let mut query = command_audit_logs::Entity::find()
            .order_by_desc(command_audit_logs::Column::CreatedAt)
            .limit(limit.max(1));
        if let Some(actor) = actor_mxid {
            query = query.filter(command_audit_logs::Column::ActorMxid.eq(actor));
        }
        let rows = query.all(&self.db).await?;
        rows.into_iter().map(to_entry).collect()
    }
}

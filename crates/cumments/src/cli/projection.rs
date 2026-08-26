//! `cumments projection ...` command handling.

use super::args::{ListProjectionRepairsArgs, ProjectionArgs, ProjectionCommand};
use super::output::print_json;
use anyhow::{Result, bail};
use cumments_core::models::ProjectionRepairStatus;
use cumments_core::ports::ProjectionRepairStore;

/// Handles `cumments projection ...`.
pub async fn handle_projection_command(
    store: &cumments_store::DbStore,
    args: &ProjectionArgs,
) -> Result<()> {
    match &args.command {
        ProjectionCommand::ListRepairs(args) => list_repairs(store, args).await,
    }
}

async fn list_repairs(
    store: &cumments_store::DbStore,
    args: &ListProjectionRepairsArgs,
) -> Result<()> {
    let status = match args.status.as_deref() {
        None => None,
        Some("pending") => Some(ProjectionRepairStatus::Pending),
        Some("manual") => Some(ProjectionRepairStatus::Manual),
        Some("resolved") => Some(ProjectionRepairStatus::Resolved),
        Some(other) => bail!("invalid repair status {other}; use pending, manual, or resolved"),
    };
    let rows = store.list_projection_repairs(status, 0, args.limit).await?;
    print_json(&rows)?;
    Ok(())
}

//! `cumments projection-repairs ...` command handling.

use super::args::{
    EventIdArg, ListProjectionRepairsArgs, ProjectionRepairsArgs, ProjectionRepairsCommand,
};
use super::error::{CliError, CliResult};
use super::output::{print_json, print_list};
use cumments_core::models::ProjectionRepairStatus;
use cumments_core::ports::ProjectionRepairStore;

pub async fn handle_projection_repairs_command(
    store: &cumments_store::DbStore,
    args: &ProjectionRepairsArgs,
) -> CliResult<()> {
    match &args.command {
        ProjectionRepairsCommand::List(args) => list_repairs(store, args).await,
        ProjectionRepairsCommand::Get(args) => get_repair(store, args).await,
        ProjectionRepairsCommand::Retry(args) => retry_repair(store, args).await,
    }
}

fn parse_status(value: Option<&str>) -> CliResult<Option<ProjectionRepairStatus>> {
    match value {
        None => Ok(None),
        Some("pending") => Ok(Some(ProjectionRepairStatus::Pending)),
        Some("manual") => Ok(Some(ProjectionRepairStatus::Manual)),
        Some("resolved") => Ok(Some(ProjectionRepairStatus::Resolved)),
        Some(other) => Err(CliError::validation(format!(
            "invalid repair status `{other}`; use pending, manual, or resolved"
        ))),
    }
}

async fn list_repairs(
    store: &cumments_store::DbStore,
    args: &ListProjectionRepairsArgs,
) -> CliResult<()> {
    let status = parse_status(args.status.as_deref())?;
    let page = args.page.max(1);
    let per_page = args.per_page.clamp(1, 100);
    let offset = (page - 1).unsigned_abs() * per_page as u64;
    let total = store
        .count_projection_repairs(status)
        .await
        .map_err(CliError::from)?;
    let rows = store
        .list_projection_repairs(status, offset, per_page as u64)
        .await
        .map_err(CliError::from)?;
    if args.table {
        println!(
            "{:<40} {:<12} {:<24} NEXT RETRY",
            "TARGET_EVENT", "STATUS", "UPDATED"
        );
        for row in rows {
            println!(
                "{:<40} {:<12} {:<24} {}",
                row.target_event_id,
                row.status.as_str(),
                row.updated_at.to_rfc3339(),
                row.next_retry_at.to_rfc3339()
            );
        }
        return Ok(());
    }
    print_list(&rows, total as i64, page, per_page)?;
    Ok(())
}

async fn get_repair(store: &cumments_store::DbStore, args: &EventIdArg) -> CliResult<()> {
    let repair = store
        .get_projection_repair(&args.target_event_id)
        .await
        .map_err(CliError::from)?
        .ok_or_else(|| {
            CliError::not_found(format!(
                "projection repair {} not found",
                args.target_event_id
            ))
        })?;
    print_json(&repair)?;
    Ok(())
}

async fn retry_repair(store: &cumments_store::DbStore, args: &EventIdArg) -> CliResult<()> {
    let existing = store
        .get_projection_repair(&args.target_event_id)
        .await
        .map_err(CliError::from)?
        .ok_or_else(|| {
            CliError::not_found(format!(
                "projection repair {} not found",
                args.target_event_id
            ))
        })?;
    if existing.status == ProjectionRepairStatus::Resolved {
        return Err(CliError::conflict(
            "resolved projection repairs cannot be retried",
        ));
    }
    let repair = store
        .retry_projection_repair(&args.target_event_id)
        .await
        .map_err(CliError::from)?;
    print_json(&serde_json::json!({
        "data": repair,
        "meta": { "status": "accepted" },
    }))?;
    Ok(())
}

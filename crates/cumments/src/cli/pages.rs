//! `cumments pages ...` command handling.

use super::args::{CreatePageUpgradeArgs, PageIdArgs, PagesArgs, PagesCommand, RetirePageArgs};
use super::error::{CliError, CliResult};
use super::output::print_json;
use super::sites::{management_error, validation_error};
use cumments_core::models::{PageSlug, RoomStatus, SiteId};
use cumments_core::ports::RegistryStore;
use cumments_core::site_service::SiteService;

pub async fn handle_pages_command(
    store: &cumments_store::DbStore,
    driver: &dyn cumments_core::ports::MatrixDriver,
    site_service: &SiteService,
    args: &PagesArgs,
) -> CliResult<()> {
    match &args.command {
        PagesCommand::Upgrades(args) => {
            let super::args::PageUpgradesCommand::Create(args) = &args.command;
            create_upgrade(store, driver, site_service, args).await
        }
        PagesCommand::Retirements(args) => match &args.command {
            super::args::PageRetirementsCommand::Create(args) => {
                create_retirement(store, args).await
            }
            super::args::PageRetirementsCommand::Show(args) => show_retirement(store, args).await,
        },
    }
}

async fn create_upgrade(
    store: &cumments_store::DbStore,
    driver: &dyn cumments_core::ports::MatrixDriver,
    site_service: &SiteService,
    args: &CreatePageUpgradeArgs,
) -> CliResult<()> {
    let (site_id, page_slug) = validate_page(&args.site_id, &args.page_slug)?;
    let replacement = cumments_core::management::upgrade_site_page_room(
        driver,
        store,
        site_service,
        &site_id,
        &page_slug,
        &args.new_version,
    )
    .await
    .map_err(management_error)?;
    print_json(&serde_json::json!({
        "room_id": store.get_registered_room(&site_id, &page_slug)
            .await?
            .ok_or_else(|| CliError::not_found("no active comment room for this page"))?,
        "new_version": args.new_version,
        "replacement_room": replacement,
    }))?;
    Ok(())
}

async fn create_retirement(
    store: &cumments_store::DbStore,
    args: &RetirePageArgs,
) -> CliResult<()> {
    if !args.yes {
        return Err(CliError::confirmation("retiring a page requires `--yes`"));
    }
    let (site_id, page_slug) = validate_page(&args.site_id, &args.page_slug)?;
    let retired = cumments_core::management::retire_page_room(store, &site_id, &page_slug)
        .await
        .map_err(management_error)?;
    if !retired {
        return Err(CliError::not_found(
            "no active room registered for this post",
        ));
    }

    if args.wait {
        for _ in 0..300 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if store
                .get_registered_room(&site_id, &page_slug)
                .await?
                .is_none()
            {
                print_retirement(&site_id, &page_slug, "retired")?;
                return Ok(());
            }
        }
        return Err(CliError::conflict(
            "timed out waiting for the retirement to finish",
        ));
    }

    print_retirement(&site_id, &page_slug, "retiring")?;
    eprintln!(
        "The running server retires the Matrix room in the background; \
         re-run with `--wait` to block until it finishes."
    );
    Ok(())
}

async fn show_retirement(store: &cumments_store::DbStore, args: &PageIdArgs) -> CliResult<()> {
    let (site_id, page_slug) = validate_page(&args.site_id, &args.page_slug)?;
    if store
        .get_registered_room(&site_id, &page_slug)
        .await?
        .is_some()
    {
        return Err(CliError::not_found("no retirement in progress"));
    }
    for room_id in store.list_retired_rooms().await? {
        let Some(identity) = store.get_registered_room_identity(&room_id).await? else {
            continue;
        };
        if identity.site_id == site_id.as_str() && identity.page_slug == page_slug.as_str() {
            print_retirement(&site_id, &page_slug, RoomStatus::Retired.as_str())?;
            return Ok(());
        }
    }
    Err(CliError::not_found("no retirement in progress"))
}

fn print_retirement(site_id: &SiteId, page_slug: &PageSlug, state: &str) -> CliResult<()> {
    print_json(&serde_json::json!({
        "target_type": "page",
        "target_id": format!("{}/{}", site_id.as_str(), page_slug.as_str()),
        "state": state,
    }))
    .map_err(CliError::from)
}

fn validate_page(site_id: &str, page_slug: &str) -> CliResult<(SiteId, PageSlug)> {
    Ok((
        SiteId::new(site_id.to_string()).map_err(validation_error("invalid site id"))?,
        PageSlug::new(page_slug.to_string()).map_err(validation_error("invalid page slug"))?,
    ))
}

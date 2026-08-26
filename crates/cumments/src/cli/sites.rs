//! `cumments sites ...` command handling.

use super::args::{
    AddStickerArgs, ExportConfigArgs, PageUserIdArg, RemoveStickerArgs, RetireSiteArgs,
    SiteUserIdArg, SitesArgs, SitesCommand,
};
use super::error::{CliError, CliResult};
use super::output::{print_json, print_site_table};
use super::registration::generate_token;
use cumments_core::governance::{
    MANAGER_LEVEL, MODERATOR_LEVEL, SITE_ADMIN_LEVEL, validate_governance_user_id,
};
use cumments_core::management::ManagementError;
use cumments_core::models::{PageSlug, SiteId};
use cumments_core::operator::{
    OperatorListQuery, config_snippet_toml, list_operator_sites, operator_site,
};
use cumments_core::ports::{MatrixDriver, RegistryStore, SiteAuthStore};
use cumments_core::site_auth::{Origin, SiteAuthMode, SiteAuthPolicy, register_site, token_hash};
use cumments_core::site_service::SiteService;
use cumments_core::sticker_packs::{
    AddStickerInput, StickerPackUseCaseError, add_site_sticker, pack_response_shape,
    remove_site_sticker,
};
use std::fmt::Display;

pub(super) fn management_error(error: ManagementError) -> CliError {
    use ManagementError as Error;
    match error {
        Error::InvalidUserId(_)
        | Error::InvalidSiteId(_)
        | Error::InvalidPageSlug(_)
        | Error::InvalidRoomVersion(_) => CliError::validation(error.to_string()),
        Error::RoleNotFound
        | Error::RoomNotRegistered(_)
        | Error::RoomNotActive(_)
        | Error::SiteNotApiRegistered(_) => CliError::not_found(error.to_string()),
        Error::SiteLevelRoleConflict | Error::RoomVersionNotNewer(_, _) => {
            CliError::conflict(error.to_string())
        }
        Error::RoomWithoutCreateEvent(_) => CliError::conflict(error.to_string()),
        Error::Infra(source) => CliError::dependency("management operation failed", source),
    }
}

fn sticker_error(error: StickerPackUseCaseError) -> CliError {
    match error {
        StickerPackUseCaseError::Invalid(_) => CliError::validation(error.to_string()),
        StickerPackUseCaseError::SiteNotFound(_)
        | StickerPackUseCaseError::SiteWithoutSpace(_)
        | StickerPackUseCaseError::PackNotFound(_) => CliError::not_found(error.to_string()),
        StickerPackUseCaseError::Other(source) => {
            CliError::dependency("sticker operation failed", source)
        }
    }
}

pub(super) fn validation_error<E: Display>(
    prefix: &'static str,
) -> impl Fn(E) -> CliError + 'static {
    move |error| CliError::validation(format!("{prefix}: {error}"))
}

pub async fn handle_sites_command(
    store: &cumments_store::DbStore,
    driver: &dyn MatrixDriver,
    site_service: &SiteService,
    policy: &SiteAuthPolicy,
    args: &SitesArgs,
) -> CliResult<()> {
    match &args.command {
        SitesCommand::Register(register_args) => {
            let claim_token = generate_token();
            match &register_args.site_id {
                Some(site_id) => {
                    let site_id = SiteId::new(site_id.clone())
                        .map_err(validation_error("invalid site id"))?;
                    store
                        .register_site(site_id.as_str(), &token_hash(&claim_token), true)
                        .await
                        .map_err(|error| {
                            CliError::dependency("site registration failed", anyhow::anyhow!(error))
                        })?;
                    print_json(&serde_json::json!({
                        "site_id": site_id.as_str(),
                        "claim_token": claim_token,
                    }))?;
                }
                None => {
                    let registered = register_site(store).await.map_err(|error| {
                        CliError::dependency("site registration failed", anyhow::anyhow!(error))
                    })?;
                    print_json(&serde_json::json!({
                        "site_id": registered.site_id,
                        "claim_token": registered.claim_token,
                    }))?;
                }
            }
            eprintln!("Keep the claim token private: it proves ownership of this site.");
            Ok(())
        }
        SitesCommand::List(list_args) => {
            let query = OperatorListQuery {
                page: Some(list_args.page),
                per_page: Some(list_args.per_page),
                site_id: list_args.site_id.clone(),
            };
            let page = list_operator_sites(store, policy, &query).await?;
            if list_args.table {
                print_site_table(&page.data);
            } else {
                print_json(&page)?;
            }
            Ok(())
        }
        SitesCommand::Get(args) => {
            let info = store
                .get_site_auth(&args.site_id)
                .await?
                .ok_or_else(|| CliError::not_found("site not found"))?;
            print_json(&operator_site(&info, policy.entry(&args.site_id)))?;
            Ok(())
        }
        SitesCommand::Origins(args) => {
            let super::args::SiteOriginsCommand::Revoke(args) = &args.command;
            let site_id =
                SiteId::new(args.site_id.clone()).map_err(validation_error("invalid site id"))?;
            let origin = Origin::parse(&args.origin).map_err(|error| {
                CliError::validation(format!("invalid origin `{}`: {error}", args.origin))
            })?;
            if policy
                .entry(site_id.as_str())
                .is_some_and(|entry| entry.allowed_origins.iter().any(|p| p.matches(&origin)))
            {
                return Err(CliError::conflict(
                    "origin is declared in the `[sites]` configuration; edit the config file to revoke it",
                ));
            }
            let revoked = store
                .revoke_verified_origin(site_id.as_str(), &origin)
                .await?;
            if !revoked {
                return Err(CliError::not_found("origin is not verified for this site"));
            }
            let info = store
                .get_site_auth(site_id.as_str())
                .await?
                .ok_or_else(|| CliError::not_found("site not found"))?;
            print_json(&operator_site(&info, policy.entry(site_id.as_str())))?;
            Ok(())
        }
        SitesCommand::Secrets(args) => match &args.command {
            super::args::SiteSecretsCommand::Rotate(args) => {
                let site_id = SiteId::new(args.site_id.clone())
                    .map_err(validation_error("invalid site id"))?;
                if policy
                    .entry(site_id.as_str())
                    .is_some_and(|entry| entry.auth_mode == Some(SiteAuthMode::Secret))
                {
                    return Err(CliError::conflict(
                        "site secret is configured in `[sites]`; edit the config file to rotate it",
                    ));
                }
                if store.get_site_auth(site_id.as_str()).await?.is_none() {
                    return Err(CliError::not_found("site not found"));
                }
                let secret = generate_token();
                store.store_site_secret(site_id.as_str(), &secret).await?;
                print_json(&serde_json::json!({
                    "site_id": site_id.as_str(),
                    "secret": secret,
                }))?;
                eprintln!("Store the secret in the site backend; it will not be shown again.");
                Ok(())
            }
            super::args::SiteSecretsCommand::Revoke(args) => {
                if !args.yes {
                    return Err(CliError::confirmation(
                        "revoking the site secret requires `--yes`",
                    ));
                }
                let site_id = SiteId::new(args.site_id.clone())
                    .map_err(validation_error("invalid site id"))?;
                if policy
                    .entry(site_id.as_str())
                    .is_some_and(|entry| entry.secret.is_some())
                {
                    return Err(CliError::conflict(
                        "site secret is configured in `[sites]`; edit the config file to revoke it",
                    ));
                }
                let cleared = store.clear_site_secret(site_id.as_str()).await?;
                if !cleared {
                    return Err(CliError::not_found("site not found"));
                }
                print_json(&serde_json::json!({
                    "site_id": site_id.as_str(),
                    "auth_mode": SiteAuthMode::Origin.as_str(),
                }))?;
                Ok(())
            }
        },
        SitesCommand::ExportConfig(args) => export_config(store, policy, args).await,
        SitesCommand::ClaimTokens(args) => {
            let super::args::SiteClaimTokensCommand::Rotate(args) = &args.command;
            let site_id =
                SiteId::new(args.site_id.clone()).map_err(validation_error("invalid site id"))?;
            let claim_token =
                cumments_core::management::rotate_claim_token(store, site_id.as_str())
                    .await
                    .map_err(management_error)?
                    .ok_or_else(|| CliError::not_found("site not found"))?;
            print_json(&serde_json::json!({
                "site_id": site_id.as_str(),
                "claim_token": claim_token,
            }))?;
            eprintln!("Keep the new claim token private; it proves ownership of this site.");
            Ok(())
        }
        SitesCommand::Admins(args) => match &args.command {
            super::args::SiteAdminsCommand::Add(args) => {
                add_role_claim(store, args, SITE_ADMIN_LEVEL).await
            }
            super::args::SiteAdminsCommand::Remove(args) => {
                remove_role_claim(store, driver, site_service, args, SITE_ADMIN_LEVEL).await
            }
        },
        SitesCommand::Managers(args) => match &args.command {
            super::args::SiteManagersCommand::Add(args) => {
                add_role_claim(store, args, MANAGER_LEVEL).await
            }
            super::args::SiteManagersCommand::Remove(args) => {
                remove_role_claim(store, driver, site_service, args, MANAGER_LEVEL).await
            }
        },
        SitesCommand::Moderators(args) => match &args.command {
            super::args::PageModeratorsCommand::Add(args) => add_moderator_claim(store, args).await,
            super::args::PageModeratorsCommand::Remove(args) => {
                remove_room_moderator(store, driver, args).await
            }
        },
        SitesCommand::Owners(args) => {
            let super::args::SiteOwnersCommand::Transfer(args) = &args.command;
            transfer_owner(store, args).await
        }
        SitesCommand::Packs(args) => {
            let super::args::SitePacksCommand::Stickers(stickers) = &args.command;
            match &stickers.command {
                super::args::SitePackStickersCommand::Add(args) => {
                    add_sticker(store, driver, args).await
                }
                super::args::SitePackStickersCommand::Remove(args) => {
                    remove_sticker(store, driver, args).await
                }
            }
        }
        SitesCommand::Retirements(args) => match &args.command {
            super::args::SiteRetirementsCommand::Create(args) => {
                retire_site(store, policy, args).await
            }
            super::args::SiteRetirementsCommand::Show(args) => {
                show_site_retirement(store, args).await
            }
        },
    }
}

async fn show_site_retirement(
    store: &cumments_store::DbStore,
    args: &super::args::SiteIdArg,
) -> CliResult<()> {
    let info = store
        .get_site_auth(&args.site_id)
        .await?
        .ok_or_else(|| CliError::not_found("site not found"))?;
    if !matches!(
        info.lifecycle,
        cumments_core::site_auth::SiteLifecycle::Retiring
            | cumments_core::site_auth::SiteLifecycle::Retired
    ) {
        return Err(CliError::not_found("no retirement in progress"));
    }
    print_json(&serde_json::json!({
        "target_type": "site",
        "target_id": args.site_id,
        "state": info.lifecycle.as_str(),
    }))?;
    Ok(())
}

/// Starts an ownership transfer through the shared core use case and prints
/// the one-time verification token exactly like the other role claims.
async fn transfer_owner(store: &cumments_store::DbStore, args: &SiteUserIdArg) -> CliResult<()> {
    let site_id = SiteId::new(args.site_id.clone()).map_err(validation_error("invalid site id"))?;
    require_api_registered_site(store, site_id.as_str()).await?;
    let (pending, transfer) = cumments_core::management::start_owner_transfer(
        store,
        store,
        store,
        site_id.as_str(),
        &args.user_id,
    )
    .await
    .map_err(management_error)?;
    print_json(&serde_json::json!({
        "pending": true,
        "user_id": pending.user_id,
        "level": pending.level,
        "verify_token": pending.verify_token,
        "expires_at": pending.expires_at,
        "transfer": {
            "site_id": site_id.as_str(),
            "target_mxid": transfer.target_mxid,
            "status": transfer.status.as_str(),
            "expires_at": transfer.expires_at,
        },
    }))?;
    eprintln!(
        "The target MXID must DM `cumments-claim:{}` to the AS bot to complete the transfer.",
        pending.verify_token
    );
    Ok(())
}

/// Prints the JSON-wrapped config snippet (or raw TOML with `--raw`).
async fn export_config(
    store: &cumments_store::DbStore,
    policy: &SiteAuthPolicy,
    args: &ExportConfigArgs,
) -> CliResult<()> {
    let site_id = SiteId::new(args.site_id.clone()).map_err(validation_error("invalid site id"))?;
    let info = store
        .get_site_auth(site_id.as_str())
        .await?
        .ok_or_else(|| CliError::not_found("site not found"))?;
    let toml = config_snippet_toml(site_id.as_str(), &info, policy.entry(site_id.as_str()));
    if args.raw {
        print!("{toml}");
    } else {
        print_json(&serde_json::json!({
            "site_id": site_id.as_str(),
            "toml": toml,
        }))?;
    }
    Ok(())
}

/// Mirrors the governance API's POST: stores a pending claim and prints the
/// one-time verify token without touching Matrix power levels.
async fn add_role_claim(
    store: &cumments_store::DbStore,
    args: &SiteUserIdArg,
    level: i64,
) -> CliResult<()> {
    let site_id = SiteId::new(args.site_id.clone()).map_err(validation_error("invalid site id"))?;
    require_api_registered_site(store, site_id.as_str()).await?;
    let pending = cumments_core::management::create_role_claim(
        store,
        site_id.as_str(),
        "",
        &args.user_id,
        level,
    )
    .await
    .map_err(management_error)?;
    print_json(&serde_json::json!({
        "pending": true,
        "user_id": pending.user_id,
        "level": pending.level,
        "verify_token": pending.verify_token,
        "expires_at": pending.expires_at,
    }))?;
    eprintln!(
        "The target MXID must DM `cumments-claim:{}` to the AS bot to activate the role.",
        pending.verify_token
    );
    Ok(())
}

/// Removes a site-level role through the shared management use case: cancels
/// a pending claim, or removes an applied role from the Space power levels.
async fn remove_role_claim(
    store: &cumments_store::DbStore,
    driver: &dyn MatrixDriver,
    site_service: &SiteService,
    args: &SiteUserIdArg,
    level: i64,
) -> CliResult<()> {
    let site_id = SiteId::new(args.site_id.clone()).map_err(validation_error("invalid site id"))?;
    require_api_registered_site(store, site_id.as_str()).await?;
    cumments_core::management::remove_site_role(
        store,
        store,
        driver,
        site_service,
        site_id.as_str(),
        &args.user_id,
        level,
    )
    .await
    .map_err(management_error)?;
    print_json(&serde_json::json!({
        "revoked": true,
        "user_id": args.user_id,
        "level": level,
    }))?;
    Ok(())
}

/// Mirrors the page moderator claim endpoint for a registered comment room.
async fn add_moderator_claim(
    store: &cumments_store::DbStore,
    args: &PageUserIdArg,
) -> CliResult<()> {
    let (site_id, room_id) = resolve_page_room(store, args).await?;
    let pending = cumments_core::management::create_role_claim(
        store,
        site_id.as_str(),
        &room_id,
        &args.user_id,
        MODERATOR_LEVEL,
    )
    .await
    .map_err(management_error)?;
    print_json(&serde_json::json!({
        "pending": true,
        "user_id": pending.user_id,
        "level": pending.level,
        "verify_token": pending.verify_token,
        "expires_at": pending.expires_at,
    }))?;
    eprintln!(
        "The target MXID must DM `cumments-claim:{}` to the AS bot to activate the role.",
        pending.verify_token
    );
    Ok(())
}

async fn remove_room_moderator(
    store: &cumments_store::DbStore,
    driver: &dyn MatrixDriver,
    args: &PageUserIdArg,
) -> CliResult<()> {
    let (site_id, room_id) = resolve_page_room(store, args).await?;
    cumments_core::management::remove_room_moderator(
        store,
        store,
        driver,
        site_id.as_str(),
        &room_id,
        &args.user_id,
    )
    .await
    .map_err(management_error)?;
    print_json(&serde_json::json!({
        "revoked": true,
        "user_id": args.user_id,
        "level": MODERATOR_LEVEL,
    }))?;
    Ok(())
}

async fn add_sticker(
    store: &cumments_store::DbStore,
    driver: &dyn MatrixDriver,
    args: &AddStickerArgs,
) -> CliResult<()> {
    require_api_registered_site(store, &args.site_id).await?;
    let info = match &args.info {
        Some(raw) => Some(serde_json::from_str(raw).map_err(|error| {
            CliError::validation(format!("invalid sticker info JSON: {error}"))
        })?),
        None => None,
    };
    let projection = add_site_sticker(
        store,
        driver,
        AddStickerInput {
            site_id: &args.site_id,
            pack_id: &args.pack_id,
            shortcode: &args.shortcode,
            url: &args.url,
            body: args.body.clone(),
            info,
        },
    )
    .await
    .map_err(sticker_error)?;
    print_json(&pack_response_shape(&projection.pack, |_| None, |_| None))?;
    Ok(())
}

async fn remove_sticker(
    store: &cumments_store::DbStore,
    driver: &dyn MatrixDriver,
    args: &RemoveStickerArgs,
) -> CliResult<()> {
    require_api_registered_site(store, &args.site_id).await?;
    let projection =
        remove_site_sticker(store, driver, &args.site_id, &args.pack_id, &args.shortcode)
            .await
            .map_err(sticker_error)?;
    print_json(&pack_response_shape(&projection.pack, |_| None, |_| None))?;
    Ok(())
}

async fn resolve_page_room(
    store: &cumments_store::DbStore,
    args: &PageUserIdArg,
) -> CliResult<(SiteId, String)> {
    let site_id = SiteId::new(args.site_id.clone()).map_err(validation_error("invalid site id"))?;
    let page_slug =
        PageSlug::new(args.page_slug.clone()).map_err(validation_error("invalid page slug"))?;
    validate_governance_user_id(&args.user_id).map_err(validation_error("invalid user id"))?;
    require_api_registered_site(store, site_id.as_str()).await?;
    let room_id = store
        .get_registered_room(&site_id, &page_slug)
        .await?
        .ok_or_else(|| CliError::not_found("no active comment room for this page"))?;
    Ok((site_id, room_id))
}

async fn require_api_registered_site(
    store: &cumments_store::DbStore,
    site_id: &str,
) -> CliResult<()> {
    if store.get_site_auth(site_id).await?.is_none() {
        return Err(CliError::unauthorized(
            "site is not API-registered; operator-configured sites are managed through configuration",
        ));
    }
    Ok(())
}

/// Marks a site `retiring` (writes stop immediately). The running server's
/// reconciler performs the Matrix retirement and local cleanup.
async fn retire_site(
    store: &cumments_store::DbStore,
    policy: &SiteAuthPolicy,
    args: &RetireSiteArgs,
) -> CliResult<()> {
    let confirmed = args.confirm_site_id.as_deref() == Some(args.site_id.as_str());
    if !args.yes || !confirmed {
        return Err(CliError::confirmation(
            "retiring a site requires `--yes --confirm-site-id SITE_ID`",
        ));
    }
    let site_id = SiteId::new(args.site_id.clone()).map_err(validation_error("invalid site id"))?;
    if policy.entry(site_id.as_str()).is_some() {
        return Err(CliError::conflict(
            "site is declared in the `[sites]` configuration; remove it from the config file instead",
        ));
    }
    if !cumments_core::management::retire_site(store, site_id.as_str())
        .await
        .map_err(management_error)?
    {
        return Err(CliError::not_found("site not found or already retired"));
    }

    if args.wait {
        for _ in 0..300 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if store.get_site_auth(site_id.as_str()).await?.is_none() {
                print_json(&serde_json::json!({
                    "site_id": site_id.as_str(),
                    "status": "retired",
                }))?;
                return Ok(());
            }
        }
        return Err(CliError::conflict(
            "timed out waiting for the retirement to finish",
        ));
    }

    print_json(&serde_json::json!({
        "site_id": site_id.as_str(),
        "status": "retiring",
    }))?;
    eprintln!(
        "The running server retires the Matrix Space/rooms in the background; \
         re-run with `--wait` to block until it finishes."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::args::{
        AddStickerArgs, ExportConfigArgs, PageUserIdArg, RemoveStickerArgs, RetireSiteArgs,
        RevokeOriginArgs, RevokeSecretArgs, SiteClaimTokensArgs, SiteClaimTokensCommand, SiteIdArg,
        SiteListArgs, SiteOriginsArgs, SitePacksArgs, SiteRetirementsArgs, SiteRetirementsCommand,
        SiteSecretsArgs, SiteUserIdArg, SitesArgs, SitesCommand,
    };
    use super::super::test_support::*;
    use super::*;
    use cumments_core::ports::{RoleClaimStore, SiteStore};
    use cumments_core::site_auth::OriginPattern;
    use cumments_core::site_auth::SiteLifecycle;
    use cumments_store::DbStore;

    async fn run_sites(
        store: &DbStore,
        policy: &SiteAuthPolicy,
        args: &SitesArgs,
    ) -> super::super::error::CliResult<()> {
        let driver = cumments_matrix::LoggingMatrixDriver;
        let site_service = SiteService::new(std::sync::Arc::new(store.clone())
            as std::sync::Arc<dyn cumments_core::ports::SiteStore>);
        handle_sites_command(store, &driver, &site_service, policy, args).await
    }

    #[tokio::test]
    async fn sites_management_lifecycle() {
        let store = DbStore::connect(&test_db_url("sites"))
            .await
            .expect("connect db");
        let policy = test_policy();
        store
            .register_site("my-blog", &token_hash("old-token"), true)
            .await
            .expect("register site");

        let rotate = SitesArgs {
            command: SitesCommand::Secrets(SiteSecretsArgs {
                command: super::super::args::SiteSecretsCommand::Rotate(SiteIdArg {
                    site_id: "my-blog".to_string(),
                }),
            }),
        };
        run_sites(&store, &policy, &rotate)
            .await
            .expect("rotate secret");
        let auth = store
            .get_site_auth("my-blog")
            .await
            .expect("load site")
            .expect("site exists");
        assert!(auth.secret.is_some(), "secret must be stored");

        let revoke_unconfirmed = SitesArgs {
            command: SitesCommand::Secrets(SiteSecretsArgs {
                command: super::super::args::SiteSecretsCommand::Revoke(RevokeSecretArgs {
                    site_id: "my-blog".to_string(),
                    yes: false,
                }),
            }),
        };
        assert!(
            run_sites(&store, &policy, &revoke_unconfirmed)
                .await
                .is_err(),
            "revoke-secret must require --yes"
        );

        let revoke = SitesArgs {
            command: SitesCommand::Secrets(SiteSecretsArgs {
                command: super::super::args::SiteSecretsCommand::Revoke(RevokeSecretArgs {
                    site_id: "my-blog".to_string(),
                    yes: true,
                }),
            }),
        };
        run_sites(&store, &policy, &revoke)
            .await
            .expect("revoke secret");
        let auth = store
            .get_site_auth("my-blog")
            .await
            .expect("load site")
            .expect("site exists");
        assert!(auth.secret.is_none(), "secret must be cleared");

        let old_hash = store
            .get_claim_token_hash("my-blog")
            .await
            .expect("old hash")
            .expect("hash exists");
        let rotate_claim = SitesArgs {
            command: SitesCommand::ClaimTokens(SiteClaimTokensArgs {
                command: SiteClaimTokensCommand::Rotate(SiteIdArg {
                    site_id: "my-blog".to_string(),
                }),
            }),
        };
        run_sites(&store, &policy, &rotate_claim)
            .await
            .expect("rotate claim token");
        let new_hash = store
            .get_claim_token_hash("my-blog")
            .await
            .expect("new hash")
            .expect("hash exists");
        assert_ne!(old_hash, new_hash, "claim token hash must rotate");
    }

    #[tokio::test]
    async fn revoke_origin_and_export_config_work() {
        let store = DbStore::connect(&test_db_url("origin"))
            .await
            .expect("connect db");
        let policy = test_policy();
        store
            .register_site("my-blog", &token_hash("token"), true)
            .await
            .expect("register site");
        let origin = Origin::parse("https://blog.example.com").expect("parse origin");
        store
            .add_verified_origin("my-blog", &origin)
            .await
            .expect("add origin");

        let revoke = SitesArgs {
            command: SitesCommand::Origins(SiteOriginsArgs {
                command: super::super::args::SiteOriginsCommand::Revoke(RevokeOriginArgs {
                    site_id: "my-blog".to_string(),
                    origin: "https://blog.example.com".to_string(),
                }),
            }),
        };
        run_sites(&store, &policy, &revoke)
            .await
            .expect("revoke origin");
        let auth = store
            .get_site_auth("my-blog")
            .await
            .expect("load site")
            .expect("site exists");
        assert!(auth.verified_origins.is_empty());

        let export = SitesArgs {
            command: SitesCommand::ExportConfig(ExportConfigArgs {
                site_id: "my-blog".to_string(),
                raw: false,
            }),
        };
        run_sites(&store, &policy, &export)
            .await
            .expect("export config snippet");
    }

    #[tokio::test]
    async fn governance_claims_are_pending_and_revocable() {
        let store = DbStore::connect(&test_db_url("gov-cli"))
            .await
            .expect("connect db");
        let policy = test_policy();
        store
            .register_site("my-blog", &token_hash("token"), true)
            .await
            .expect("register site");

        let add = SitesArgs {
            command: SitesCommand::Admins(super::super::args::SiteAdminsArgs {
                command: super::super::args::SiteAdminsCommand::Add(SiteUserIdArg {
                    site_id: "my-blog".to_string(),
                    user_id: "@owner:hs".to_string(),
                }),
            }),
        };
        run_sites(&store, &policy, &add)
            .await
            .expect("add admin claim");
        assert_eq!(
            store
                .pending_claims_for_user("@owner:hs")
                .await
                .expect("pending claims")
                .len(),
            1
        );

        let remove = SitesArgs {
            command: SitesCommand::Admins(super::super::args::SiteAdminsArgs {
                command: super::super::args::SiteAdminsCommand::Remove(SiteUserIdArg {
                    site_id: "my-blog".to_string(),
                    user_id: "@owner:hs".to_string(),
                }),
            }),
        };
        run_sites(&store, &policy, &remove)
            .await
            .expect("remove admin claim");
        assert!(
            store
                .pending_claims_for_user("@owner:hs")
                .await
                .expect("pending claims")
                .is_empty()
        );

        // Applied roles are Matrix state, which the CLI does not write.
        let missing = SitesArgs {
            command: SitesCommand::Admins(super::super::args::SiteAdminsArgs {
                command: super::super::args::SiteAdminsCommand::Remove(SiteUserIdArg {
                    site_id: "my-blog".to_string(),
                    user_id: "@owner:hs".to_string(),
                }),
            }),
        };
        assert!(run_sites(&store, &policy, &missing).await.is_err());
    }

    #[tokio::test]
    async fn sites_list_merges_config_only_sites() {
        let store = DbStore::connect(&test_db_url("list-merge"))
            .await
            .expect("connect db");
        let mut policy = test_policy();
        policy.sites.insert(
            "config-blog".to_string(),
            cumments_core::site_auth::SitePolicyEntry {
                auth_mode: Some(SiteAuthMode::Origin),
                allowed_origins: vec![
                    OriginPattern::parse("https://blog.example.com").expect("parse pattern"),
                ],
                secret: None,
            },
        );

        let list = SitesArgs {
            command: SitesCommand::List(SiteListArgs {
                site_id: None,
                page: 1,
                per_page: 20,
                table: false,
            }),
        };
        run_sites(&store, &policy, &list)
            .await
            .expect("list sites with config overlay");
    }

    #[tokio::test]
    async fn moderator_and_sticker_management_work() {
        let store = DbStore::connect(&test_db_url("moderator-sticker"))
            .await
            .expect("connect db");
        let policy = test_policy();
        let site_id = cumments_core::models::SiteId::from("my-blog");
        let slug = cumments_core::models::PageSlug::from("hello");
        store
            .register_site("my-blog", &token_hash("token"), true)
            .await
            .expect("register site");
        store
            .ensure_site_exists("my-blog", "!space:hs")
            .await
            .expect("attach space");
        store
            .register_room("!room:hs", &site_id, &slug)
            .await
            .expect("register room");

        let add_moderator = SitesArgs {
            command: SitesCommand::Moderators(super::super::args::PageModeratorsArgs {
                command: super::super::args::PageModeratorsCommand::Add(PageUserIdArg {
                    site_id: "my-blog".to_string(),
                    page_slug: "hello".to_string(),
                    user_id: "@mod:hs".to_string(),
                }),
            }),
        };
        run_sites(&store, &policy, &add_moderator)
            .await
            .expect("add moderator claim");
        assert_eq!(
            store
                .pending_claims_for_user("@mod:hs")
                .await
                .expect("pending claims")
                .len(),
            1
        );

        let remove_moderator = SitesArgs {
            command: SitesCommand::Moderators(super::super::args::PageModeratorsArgs {
                command: super::super::args::PageModeratorsCommand::Remove(PageUserIdArg {
                    site_id: "my-blog".to_string(),
                    page_slug: "hello".to_string(),
                    user_id: "@mod:hs".to_string(),
                }),
            }),
        };
        run_sites(&store, &policy, &remove_moderator)
            .await
            .expect("remove moderator claim");
        assert!(
            store
                .pending_claims_for_user("@mod:hs")
                .await
                .expect("pending claims")
                .is_empty()
        );

        let driver = cumments_test_utils::TestDriver::new();
        let add_sticker = SitesArgs {
            command: SitesCommand::Packs(SitePacksArgs {
                command: super::super::args::SitePacksCommand::Stickers(
                    super::super::args::SitePackStickersArgs {
                        command: super::super::args::SitePackStickersCommand::Add(AddStickerArgs {
                            site_id: "my-blog".to_string(),
                            pack_id: "default".to_string(),
                            shortcode: "cat".to_string(),
                            url: "mxc://hs/cat".to_string(),
                            body: Some("a cat".to_string()),
                            info: Some(r#"{"w":10}"#.to_string()),
                        }),
                    },
                ),
            }),
        };
        let site_service_for_stickers = SiteService::new(std::sync::Arc::new(store.clone())
            as std::sync::Arc<dyn cumments_core::ports::SiteStore>);
        handle_sites_command(
            &store,
            &driver,
            &site_service_for_stickers,
            &policy,
            &add_sticker,
        )
        .await
        .expect("add sticker");

        let remove_sticker_args = SitesArgs {
            command: SitesCommand::Packs(SitePacksArgs {
                command: super::super::args::SitePacksCommand::Stickers(
                    super::super::args::SitePackStickersArgs {
                        command: super::super::args::SitePackStickersCommand::Remove(
                            RemoveStickerArgs {
                                site_id: "my-blog".to_string(),
                                pack_id: "default".to_string(),
                                shortcode: "cat".to_string(),
                            },
                        ),
                    },
                ),
            }),
        };
        handle_sites_command(
            &store,
            &driver,
            &site_service_for_stickers,
            &policy,
            &remove_sticker_args,
        )
        .await
        .expect("remove sticker");
    }

    #[tokio::test]
    async fn retire_requires_confirmation_and_marks_site_retiring() {
        let store = DbStore::connect(&test_db_url("retire"))
            .await
            .expect("connect db");
        let policy = test_policy();
        store
            .register_site("my-blog", &token_hash("token"), true)
            .await
            .expect("register site");

        let unconfirmed = SitesArgs {
            command: SitesCommand::Retirements(SiteRetirementsArgs {
                command: SiteRetirementsCommand::Create(RetireSiteArgs {
                    site_id: "my-blog".to_string(),
                    yes: false,
                    confirm_site_id: None,
                    wait: false,
                }),
            }),
        };
        assert!(
            run_sites(&store, &policy, &unconfirmed).await.is_err(),
            "retire must require explicit site confirmation"
        );

        let retire = SitesArgs {
            command: SitesCommand::Retirements(SiteRetirementsArgs {
                command: SiteRetirementsCommand::Create(RetireSiteArgs {
                    site_id: "my-blog".to_string(),
                    yes: true,
                    confirm_site_id: Some("my-blog".to_string()),
                    wait: false,
                }),
            }),
        };
        run_sites(&store, &policy, &retire)
            .await
            .expect("retire site");

        let auth = store
            .get_site_auth("my-blog")
            .await
            .expect("load site")
            .expect("site exists");
        assert_eq!(auth.lifecycle, SiteLifecycle::Retiring);
        assert!(auth.claim_token_hash.is_none());
        assert!(
            store
                .list_retiring_sites()
                .await
                .expect("retiring sites")
                .contains(&"my-blog".to_string())
        );
    }
}

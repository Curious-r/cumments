//! `cumments sites ...` command handling.

use super::args::{ExportConfigArgs, SiteUserIdArg, SitesArgs, SitesCommand};
use super::output::{print_json, print_site_table};
use super::registration::generate_token;
use anyhow::{Result, bail};
use chrono::{Duration, Utc};
use cumments_api::routes::admin::{
    AdminListQuery, AdminPage, AdminSite, admin_meta, admin_page_bounds, admin_site,
    admin_site_from_config, config_snippet_toml,
};
use cumments_core::governance::{
    CO_MANAGER_LEVEL, NewRoleClaim, OWNER_LEVEL, RoleEntry, validate_governance_user_id,
};
use cumments_core::models::SiteId;
use cumments_core::ports::{GovernanceStore, RoleClaimStore, SiteAuthStore};
use cumments_core::site_auth::{Origin, SiteAuthMode, SiteAuthPolicy, register_site, token_hash};
use std::collections::HashSet;

/// How long an unverified role claim stays valid, matching the governance API.
const ROLE_CLAIM_TTL_HOURS: i64 = 24;

pub async fn handle_sites_command(
    store: &cumments_store::DbStore,
    policy: &SiteAuthPolicy,
    args: &SitesArgs,
) -> Result<()> {
    match &args.command {
        SitesCommand::Register(register_args) => {
            let claim_token = generate_token();
            match &register_args.site_id {
                Some(site_id) => {
                    let site_id = SiteId::new(site_id.clone())
                        .map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
                    store
                        .register_site(site_id.as_str(), &token_hash(&claim_token))
                        .await?;
                    print_json(&serde_json::json!({
                        "site_id": site_id.as_str(),
                        "claim_token": claim_token,
                    }))?;
                }
                None => {
                    let registered = register_site(store).await?;
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
            let query = AdminListQuery {
                page: Some(list_args.page),
                per_page: Some(list_args.per_page),
                site_id: list_args.site_id.clone(),
            };
            let page = list_admin_sites(store, policy, &query).await?;
            if list_args.table {
                print_site_table(&page.data);
            } else {
                print_json(&page)?;
            }
            Ok(())
        }
        SitesCommand::RevokeOrigin(args) => {
            let site_id = SiteId::new(args.site_id.clone())
                .map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
            let origin = Origin::parse(&args.origin)
                .map_err(|e| anyhow::anyhow!("invalid origin `{}`: {e}", args.origin))?;
            if policy
                .entry(site_id.as_str())
                .is_some_and(|entry| entry.allowed_origins.iter().any(|p| p.matches(&origin)))
            {
                bail!(
                    "origin is declared in the `[sites]` configuration; \
                     edit the config file to revoke it"
                );
            }
            let revoked = store
                .revoke_verified_origin(site_id.as_str(), &origin)
                .await?;
            if !revoked {
                bail!("origin is not verified for this site");
            }
            let info = store
                .get_site_auth(site_id.as_str())
                .await?
                .ok_or_else(|| anyhow::anyhow!("site not found"))?;
            print_json(&admin_site(&info, policy.entry(site_id.as_str())))?;
            Ok(())
        }
        SitesCommand::RotateSecret(args) => {
            let site_id = SiteId::new(args.site_id.clone())
                .map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
            if policy
                .entry(site_id.as_str())
                .is_some_and(|entry| entry.auth_mode == Some(SiteAuthMode::Secret))
            {
                bail!(
                    "site secret is configured in `[sites]`; \
                     edit the config file to rotate it"
                );
            }
            if store.get_site_auth(site_id.as_str()).await?.is_none() {
                bail!("site not found");
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
        SitesCommand::RevokeSecret(args) => {
            if !args.yes {
                bail!("refusing to revoke the secret without `--yes`");
            }
            let site_id = SiteId::new(args.site_id.clone())
                .map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
            if policy
                .entry(site_id.as_str())
                .is_some_and(|entry| entry.secret.is_some())
            {
                bail!(
                    "site secret is configured in `[sites]`; \
                     edit the config file to revoke it"
                );
            }
            let cleared = store.clear_site_secret(site_id.as_str()).await?;
            if !cleared {
                bail!("site not found");
            }
            print_json(&serde_json::json!({
                "site_id": site_id.as_str(),
                "auth_mode": SiteAuthMode::Origin.as_str(),
            }))?;
            Ok(())
        }
        SitesCommand::ExportConfig(args) => export_config(store, policy, args).await,
        SitesCommand::RotateClaimToken(args) => {
            let site_id = SiteId::new(args.site_id.clone())
                .map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
            let claim_token = generate_token();
            let rotated = store
                .rotate_claim_token(site_id.as_str(), &token_hash(&claim_token))
                .await?;
            if !rotated {
                bail!("site not found");
            }
            print_json(&serde_json::json!({
                "site_id": site_id.as_str(),
                "claim_token": claim_token,
            }))?;
            eprintln!("Keep the new claim token private; it proves ownership of this site.");
            Ok(())
        }
        SitesCommand::AddOwner(args) => add_role_claim(store, args, OWNER_LEVEL).await,
        SitesCommand::AddCoManager(args) => add_role_claim(store, args, CO_MANAGER_LEVEL).await,
        SitesCommand::RemoveOwner(args) => remove_role_claim(store, args, OWNER_LEVEL).await,
        SitesCommand::RemoveCoManager(args) => {
            remove_role_claim(store, args, CO_MANAGER_LEVEL).await
        }
    }
}

/// Prints the JSON-wrapped config snippet (or raw TOML with `--raw`).
async fn export_config(
    store: &cumments_store::DbStore,
    policy: &SiteAuthPolicy,
    args: &ExportConfigArgs,
) -> Result<()> {
    let site_id =
        SiteId::new(args.site_id.clone()).map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
    let info = store
        .get_site_auth(site_id.as_str())
        .await?
        .ok_or_else(|| anyhow::anyhow!("site not found"))?;
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
) -> Result<()> {
    let site_id =
        SiteId::new(args.site_id.clone()).map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
    require_api_registered_site(store, site_id.as_str()).await?;
    let user_id = validate_governance_user_id(&args.user_id)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    let verify_token = generate_token();
    let claim = store
        .upsert_role_claim(&NewRoleClaim {
            site_id: site_id.as_str().to_string(),
            room_id: String::new(),
            user_id,
            level,
            token_hash: token_hash(&verify_token),
            expires_at: Utc::now() + Duration::hours(ROLE_CLAIM_TTL_HOURS),
        })
        .await?;
    print_json(&serde_json::json!({
        "pending": true,
        "user_id": claim.user_id,
        "level": claim.level,
        "verify_token": verify_token,
        "expires_at": claim.expires_at,
    }))?;
    eprintln!(
        "The target MXID must DM `cumments-claim:{verify_token}` to the AS bot to activate the role."
    );
    Ok(())
}

/// Mirrors the governance API's DELETE for the pending-claim part. Applied
/// roles are Matrix power-level state, which the CLI deliberately does not
/// write: operators remove those from a Matrix client or the admin API.
async fn remove_role_claim(
    store: &cumments_store::DbStore,
    args: &SiteUserIdArg,
    level: i64,
) -> Result<()> {
    let site_id =
        SiteId::new(args.site_id.clone()).map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
    require_api_registered_site(store, site_id.as_str()).await?;
    let user_id = validate_governance_user_id(&args.user_id)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    let revoked = store
        .revoke_role_claim(site_id.as_str(), "", &user_id, level)
        .await?;
    if !revoked {
        let applied = store
            .list_site_roles(site_id.as_str())
            .await?
            .iter()
            .any(|role: &RoleEntry| role.user_id == user_id && role.level == level);
        if applied {
            bail!(
                "role is already applied in Matrix; remove it from the Space power \
                 levels in a Matrix client or through the admin API"
            );
        }
        bail!("no pending claim or applied role for this user and level");
    }
    print_json(&serde_json::json!({
        "revoked": true,
        "user_id": user_id,
        "level": level,
    }))?;
    Ok(())
}

async fn require_api_registered_site(store: &cumments_store::DbStore, site_id: &str) -> Result<()> {
    if store.get_site_auth(site_id).await?.is_none() {
        bail!(
            "site is not API-registered; operator-configured sites are managed \
             through configuration"
        );
    }
    Ok(())
}

/// Lists managed sites, merging database rows with the `[sites]` overlay —
/// the same view the admin API returns.
async fn list_admin_sites(
    store: &cumments_store::DbStore,
    policy: &SiteAuthPolicy,
    query: &AdminListQuery,
) -> Result<AdminPage<AdminSite>> {
    let db_sites = store.list_site_auth().await?;
    let mut sites = db_sites
        .iter()
        .map(|info| admin_site(info, policy.entry(&info.site_id)))
        .collect::<Vec<_>>();
    let known = sites
        .iter()
        .map(|site| site.site_id.clone())
        .collect::<HashSet<_>>();
    for (site_id, entry) in &policy.sites {
        if !known.contains(site_id) {
            sites.push(admin_site_from_config(site_id, entry));
        }
    }
    sites.sort_by(|a, b| a.site_id.cmp(&b.site_id));
    if let Some(site_id) = query.site_id.as_deref().filter(|s| !s.is_empty()) {
        sites.retain(|site| site.site_id == site_id);
    }
    let (page, per_page) = admin_page_bounds(query);
    let total = sites.len() as i64;
    let start = ((page - 1) * per_page) as usize;
    let data = sites
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .collect();
    Ok(AdminPage {
        data,
        meta: admin_meta(total, page, per_page),
    })
}

#[cfg(test)]
mod tests {
    use super::super::args::{
        ExportConfigArgs, RevokeOriginArgs, RevokeSecretArgs, SiteIdArg, SiteListArgs,
        SiteUserIdArg,
    };
    use super::super::test_support::*;
    use super::*;
    use cumments_core::site_auth::OriginPattern;
    use cumments_store::DbStore;

    #[tokio::test]
    async fn sites_management_lifecycle() {
        let store = DbStore::connect(&test_db_url("sites"))
            .await
            .expect("connect db");
        let policy = test_policy();
        store
            .register_site("my-blog", &token_hash("old-token"))
            .await
            .expect("register site");

        let rotate = SitesArgs {
            command: SitesCommand::RotateSecret(SiteIdArg {
                site_id: "my-blog".to_string(),
            }),
        };
        handle_sites_command(&store, &policy, &rotate)
            .await
            .expect("rotate secret");
        let auth = store
            .get_site_auth("my-blog")
            .await
            .expect("load site")
            .expect("site exists");
        assert!(auth.secret.is_some(), "secret must be stored");

        let revoke_unconfirmed = SitesArgs {
            command: SitesCommand::RevokeSecret(RevokeSecretArgs {
                site_id: "my-blog".to_string(),
                yes: false,
            }),
        };
        assert!(
            handle_sites_command(&store, &policy, &revoke_unconfirmed)
                .await
                .is_err(),
            "revoke-secret must require --yes"
        );

        let revoke = SitesArgs {
            command: SitesCommand::RevokeSecret(RevokeSecretArgs {
                site_id: "my-blog".to_string(),
                yes: true,
            }),
        };
        handle_sites_command(&store, &policy, &revoke)
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
            command: SitesCommand::RotateClaimToken(SiteIdArg {
                site_id: "my-blog".to_string(),
            }),
        };
        handle_sites_command(&store, &policy, &rotate_claim)
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
            .register_site("my-blog", &token_hash("token"))
            .await
            .expect("register site");
        let origin = Origin::parse("https://blog.example.com").expect("parse origin");
        store
            .add_verified_origin("my-blog", &origin)
            .await
            .expect("add origin");

        let revoke = SitesArgs {
            command: SitesCommand::RevokeOrigin(RevokeOriginArgs {
                site_id: "my-blog".to_string(),
                origin: "https://blog.example.com".to_string(),
            }),
        };
        handle_sites_command(&store, &policy, &revoke)
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
        handle_sites_command(&store, &policy, &export)
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
            .register_site("my-blog", &token_hash("token"))
            .await
            .expect("register site");

        let add = SitesArgs {
            command: SitesCommand::AddOwner(SiteUserIdArg {
                site_id: "my-blog".to_string(),
                user_id: "@owner:hs".to_string(),
            }),
        };
        handle_sites_command(&store, &policy, &add)
            .await
            .expect("add owner claim");
        assert_eq!(
            store
                .pending_claims_for_user("@owner:hs")
                .await
                .expect("pending claims")
                .len(),
            1
        );

        let remove = SitesArgs {
            command: SitesCommand::RemoveOwner(SiteUserIdArg {
                site_id: "my-blog".to_string(),
                user_id: "@owner:hs".to_string(),
            }),
        };
        handle_sites_command(&store, &policy, &remove)
            .await
            .expect("remove owner claim");
        assert!(
            store
                .pending_claims_for_user("@owner:hs")
                .await
                .expect("pending claims")
                .is_empty()
        );

        // Applied roles are Matrix state, which the CLI does not write.
        let missing = SitesArgs {
            command: SitesCommand::RemoveOwner(SiteUserIdArg {
                site_id: "my-blog".to_string(),
                user_id: "@owner:hs".to_string(),
            }),
        };
        assert!(
            handle_sites_command(&store, &policy, &missing)
                .await
                .is_err()
        );
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
        handle_sites_command(&store, &policy, &list)
            .await
            .expect("list sites with config overlay");
    }
}

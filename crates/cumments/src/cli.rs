//! CLI subcommands for Cumments.

use anyhow::{Result, bail};
use clap::Subcommand;
use cumments_api::routes::admin::{
    AdminBlockedRoom, AdminListQuery, AdminPage, AdminSite, admin_meta, admin_page_bounds,
    admin_site, admin_site_from_config, config_snippet_toml,
};
use cumments_core::models::SiteId;
use cumments_core::ports::{RegistryStore, SiteAuthStore};
use cumments_core::site_auth::{Origin, SiteAuthMode, SiteAuthPolicy, register_site, token_hash};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use crate::config::{regex_escape, resolve_config_path};

/// Generate a complete registration.yaml for the AppService mode.
#[derive(clap::Args, Debug)]
pub struct GenerateRegistrationArgs {
    /// The URL where the homeserver can reach this Cumments instance
    #[arg(long)]
    pub url: Option<String>,

    /// The Matrix server name (domain)
    #[arg(long)]
    pub server_name: Option<String>,

    /// The localpart for the AppService's sender user
    #[arg(long)]
    pub sender_localpart: Option<String>,

    /// The AppService id (must match config's matrix.appservice.id)
    #[arg(long)]
    pub id: Option<String>,

    /// Suppress token hints; stdout carries a `[REDACTED]` YAML for
    /// demos/audits only. Use `--output` for a real registration file.
    #[arg(long, default_value_t = false)]
    pub quiet: bool,

    /// Write the registration YAML (real tokens) to this file with 0600
    /// permissions instead of printing it.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

/// Backfill the read model from Matrix room history.
#[derive(clap::Args, Debug)]
pub struct BackfillArgs {
    /// Stop fetching a room after this many history pages (~100 events each).
    /// The cursor is saved so a later run resumes where it stopped.
    /// `0` disables the cap.
    #[arg(long, default_value_t = 500)]
    pub max_pages: u32,
}

/// Create a consistent single-file SQLite backup.
#[derive(clap::Args, Debug)]
pub struct BackupArgs {
    /// Destination SQLite file (must not already exist)
    #[arg(short, long)]
    pub output: PathBuf,
}

/// Site management subcommands.
#[derive(clap::Args, Debug)]
pub struct SitesArgs {
    #[command(subcommand)]
    pub command: SitesCommand,
}

#[derive(Subcommand, Debug)]
pub enum SitesCommand {
    /// Register a site and print its id and one-time claim token
    #[command(name = "register")]
    Register(RegisterSiteArgs),
    /// List managed sites (database rows merged with the `[sites]` overlay)
    #[command(name = "list")]
    List(SiteListArgs),
    /// Revoke a verified origin (config-declared origins cannot be revoked)
    #[command(name = "revoke-origin")]
    RevokeOrigin(RevokeOriginArgs),
    /// Rotate the HMAC secret; the new secret is printed exactly once
    #[command(name = "rotate-secret")]
    RotateSecret(SiteIdArg),
    /// Remove the HMAC secret and fall back to origin auth
    #[command(name = "revoke-secret")]
    RevokeSecret(RevokeSecretArgs),
    /// Export a TOML block to adopt a database-tracked site into `[sites]`
    #[command(name = "export-config")]
    ExportConfig(SiteIdArg),
    /// Rotate the claim token; the new token is printed exactly once
    #[command(name = "rotate-claim-token")]
    RotateClaimToken(SiteIdArg),
}

#[derive(clap::Args, Debug)]
pub struct RegisterSiteArgs {
    /// Optional explicit site id (operator-chosen). Without it, a random,
    /// unguessable id is generated.
    #[arg(long)]
    pub site_id: Option<String>,
}

/// Arguments for listing sites (mirrors `QUERY /api/v1/admin/sites`).
#[derive(clap::Args, Debug)]
pub struct SiteListArgs {
    /// Only show this site id
    #[arg(long)]
    pub site_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i64,
    #[arg(long, default_value_t = 20)]
    pub per_page: i64,
    /// Render a human-readable table instead of JSON
    #[arg(long)]
    pub table: bool,
}

/// A single site id.
#[derive(clap::Args, Debug)]
pub struct SiteIdArg {
    pub site_id: String,
}

/// Arguments for revoking the HMAC secret.
#[derive(clap::Args, Debug)]
pub struct RevokeSecretArgs {
    pub site_id: String,
    /// Confirm the destructive operation
    #[arg(long)]
    pub yes: bool,
}

/// Arguments for revoking a verified origin.
#[derive(clap::Args, Debug)]
pub struct RevokeOriginArgs {
    pub site_id: String,
    pub origin: String,
}

/// Blocked room management subcommands.
#[derive(clap::Args, Debug)]
pub struct RoomsArgs {
    #[command(subcommand)]
    pub command: RoomsCommand,
}

#[derive(Subcommand, Debug)]
pub enum RoomsCommand {
    /// List rooms currently blocked from adoption
    #[command(name = "list-blocked")]
    ListBlocked(BlockedListArgs),
    /// Clear a room's blocked state so it can be adopted again
    #[command(name = "unblock")]
    Unblock(UnblockRoomArgs),
}

/// Arguments for listing blocked rooms (mirrors `QUERY /api/v1/admin/rooms/blocked`).
#[derive(clap::Args, Debug)]
pub struct BlockedListArgs {
    /// Only show rooms for this site
    #[arg(long)]
    pub site_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i64,
    #[arg(long, default_value_t = 20)]
    pub per_page: i64,
    /// Render a human-readable table instead of JSON
    #[arg(long)]
    pub table: bool,
}

/// Arguments for unblocking a room.
#[derive(clap::Args, Debug)]
pub struct UnblockRoomArgs {
    pub room_id: String,
}

/// Arguments for generating shell completions.
#[derive(clap::Args, Debug)]
pub struct CompletionsArgs {
    /// Target shell
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

/// AppService management subcommands.
#[derive(clap::Args, Debug)]
pub struct AppserviceArgs {
    #[command(subcommand)]
    pub command: AppserviceCommand,
}

#[derive(Subcommand, Debug)]
pub enum AppserviceCommand {
    /// Generate a complete registration.yaml for the AppService mode
    #[command(name = "generate-registration")]
    GenerateRegistration(GenerateRegistrationArgs),
}

/// All CLI subcommands.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// AppService management commands
    #[command(name = "appservice")]
    Appservice(AppserviceArgs),
    /// Rebuild the read model from Matrix room history
    #[command(name = "backfill")]
    Backfill(BackfillArgs),
    /// Create a consistent single-file SQLite backup
    #[command(name = "backup")]
    Backup(BackupArgs),
    /// Manage sites registered through the API
    #[command(name = "sites")]
    Sites(SitesArgs),
    /// Manage blocked comment rooms
    #[command(name = "rooms")]
    Rooms(RoomsArgs),
    /// Generate a shell completion script
    #[command(name = "completions")]
    Completions(CompletionsArgs),
}

/// The registration data model (serialised to YAML).
#[derive(Serialize)]
struct AppServiceRegistration {
    id: String,
    url: String,
    as_token: String,
    hs_token: String,
    #[serde(rename = "sender_localpart")]
    sender_localpart: String,
    #[serde(rename = "rate_limited")]
    rate_limited: bool,
    namespaces: Namespaces,
}

#[derive(Serialize)]
struct Namespaces {
    users: Vec<NamespaceRule>,
    aliases: Vec<NamespaceRule>,
    rooms: Vec<NamespaceRule>,
}

#[derive(Serialize)]
struct NamespaceRule {
    exclusive: bool,
    regex: String,
}

/// Generate a cryptographically random token.
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Handles `cumments sites ...` against the local database.
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
                    println!(
                        "{}",
                        serde_json::json!({
                            "site_id": site_id.as_str(),
                            "claim_token": claim_token,
                        })
                    );
                }
                None => {
                    let registered = register_site(store).await?;
                    println!(
                        "{}",
                        serde_json::json!({
                            "site_id": registered.site_id,
                            "claim_token": registered.claim_token,
                        })
                    );
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
            println!(
                "{}",
                serde_json::json!({ "site_id": site_id.as_str(), "secret": secret })
            );
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
        SitesCommand::ExportConfig(args) => {
            let site_id = SiteId::new(args.site_id.clone())
                .map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;
            let info = store
                .get_site_auth(site_id.as_str())
                .await?
                .ok_or_else(|| anyhow::anyhow!("site not found"))?;
            print!(
                "{}",
                config_snippet_toml(site_id.as_str(), &info, policy.entry(site_id.as_str()))
            );
            Ok(())
        }
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
            println!(
                "{}",
                serde_json::json!({ "site_id": site_id.as_str(), "claim_token": claim_token })
            );
            eprintln!("Keep the new claim token private; it proves ownership of this site.");
            Ok(())
        }
    }
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

/// Prints one JSON document to stdout (machine-readable CLI output).
fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

/// Human-readable table for `sites list --table`.
fn print_site_table(sites: &[AdminSite]) {
    println!(
        "{:<16} {:<10} {:<12} ORIGINS",
        "SITE_ID", "AUTH_MODE", "STATUS"
    );
    for site in sites {
        let origins = site
            .origins
            .iter()
            .map(|origin| origin.origin.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{:<16} {:<10} {:<12} {}",
            site.site_id,
            site.auth_mode.as_str(),
            site.verification_status.as_str(),
            origins
        );
    }
}

/// Handles `cumments rooms ...`.
pub async fn handle_rooms_command(store: &cumments_store::DbStore, args: &RoomsArgs) -> Result<()> {
    match &args.command {
        RoomsCommand::ListBlocked(list_args) => {
            let mut rooms = store.get_blocked_rooms().await?;
            rooms.sort_by(|a, b| a.site_id.cmp(&b.site_id).then(a.room_id.cmp(&b.room_id)));
            if let Some(site_id) = list_args.site_id.as_deref().filter(|s| !s.is_empty()) {
                rooms.retain(|room| room.site_id == site_id);
            }
            let query = AdminListQuery {
                page: Some(list_args.page),
                per_page: Some(list_args.per_page),
                site_id: list_args.site_id.clone(),
            };
            let (page, per_page) = admin_page_bounds(&query);
            let total = rooms.len() as i64;
            let start = ((page - 1) * per_page) as usize;
            let data = rooms
                .into_iter()
                .skip(start)
                .take(per_page as usize)
                .map(|room| AdminBlockedRoom {
                    room_id: room.room_id,
                    site_id: room.site_id,
                    post_slug: room.post_slug,
                    reason: room.reason,
                    updated_at: room.updated_at,
                })
                .collect::<Vec<_>>();
            let page = AdminPage {
                data,
                meta: admin_meta(total, page, per_page),
            };
            if list_args.table {
                print_room_table(&page.data);
            } else {
                print_json(&page)?;
            }
            Ok(())
        }
        RoomsCommand::Unblock(args) => {
            let unblocked = store.unblock_room(&args.room_id).await?;
            if !unblocked {
                bail!("room not found in the registry");
            }
            print_json(&serde_json::json!({
                "room_id": args.room_id,
                "unblocked": true,
            }))?;
            Ok(())
        }
    }
}

/// Human-readable table for `rooms list-blocked --table`.
fn print_room_table(rooms: &[AdminBlockedRoom]) {
    println!(
        "{:<44} {:<16} {:<16} REASON",
        "ROOM_ID", "SITE_ID", "POST_SLUG"
    );
    for room in rooms {
        println!(
            "{:<44} {:<16} {:<16} {}",
            room.room_id, room.site_id, room.post_slug, room.reason
        );
    }
}

/// Handles `cumments completions <shell>`.
pub fn handle_completions(args: &CompletionsArgs) -> Result<()> {
    let mut cmd = crate::cli_command();
    clap_complete::generate(args.shell, &mut cmd, "cumments", &mut std::io::stdout());
    Ok(())
}

/// Handles `cumments appservice ...`.
pub fn handle_appservice_command(args: &AppserviceArgs, config_path: Option<&str>) -> Result<()> {
    match &args.command {
        AppserviceCommand::GenerateRegistration(reg_args) => {
            handle_generate_registration(reg_args, config_path)
        }
    }
}

/// Handle the `appservice generate-registration` subcommand.
///
/// Values are resolved in this order: CLI flag, config file/environment, built-in default.
/// Only `server_name` has no built-in default: it can come from
/// `[matrix.homeserver].domain` or `--server-name`.
pub fn handle_generate_registration(
    args: &GenerateRegistrationArgs,
    config_path: Option<&str>,
) -> Result<()> {
    match resolve_config_path(config_path) {
        Some(path) => eprintln!("Using config file: {}", path.display()),
        None => eprintln!("No config file found; using CLI flags and defaults."),
    }

    let source = RegistrationSource::load(config_path)?;

    let url = args
        .url
        .clone()
        .or_else(|| source.appservice_url())
        .unwrap_or_else(|| "http://localhost:3001".to_string());
    let server_name = args
        .server_name
        .clone()
        .or_else(|| source.server_name())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "`--server-name` is required when `[matrix.homeserver].domain` is not configured"
            )
        })?;
    let sender_localpart = args
        .sender_localpart
        .clone()
        .or_else(|| source.sender_localpart())
        .unwrap_or_else(|| "_cumments_bot".to_string());
    let id = args
        .id
        .clone()
        .or_else(|| source.appservice_id())
        .unwrap_or_else(|| "cumments".to_string());

    let registration = build_registration(
        &url,
        &server_name,
        &sender_localpart,
        &id,
        args.quiet && args.output.is_none(),
    );
    let yaml = serde_yaml_ng::to_string(&registration)?;
    if let Some(output) = &args.output {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", output.display()))?;
        std::fs::write(output, yaml)?;
        eprintln!("Wrote registration to {}", output.display());
        if !args.quiet {
            eprintln!("Add the tokens from that file to cumments.toml under [matrix.appservice].");
        }
    } else {
        println!("{}", yaml);
        if !args.quiet {
            eprintln!("---");
            eprintln!("The YAML above contains tokens; redirect stdout to registration.yaml");
            eprintln!("and keep the file private.");
        }
    }

    Ok(())
}

fn build_registration(
    url: &str,
    server_name: &str,
    sender_localpart: &str,
    id: &str,
    quiet: bool,
) -> AppServiceRegistration {
    let as_token = generate_token();
    let hs_token = generate_token();

    AppServiceRegistration {
        id: id.to_string(),
        url: url.to_string(),
        as_token: if quiet {
            "[REDACTED]".to_string()
        } else {
            as_token.clone()
        },
        hs_token: if quiet {
            "[REDACTED]".to_string()
        } else {
            hs_token.clone()
        },
        sender_localpart: sender_localpart.to_string(),
        rate_limited: false,
        namespaces: Namespaces {
            users: vec![NamespaceRule {
                exclusive: true,
                regex: format!("@_cumments_.*:{}", regex_escape(server_name)),
            }],
            aliases: vec![NamespaceRule {
                exclusive: true,
                regex: format!("#_cumments_.*:{}", regex_escape(server_name)),
            }],
            rooms: vec![],
        },
    }
}

/// A partial view of the configuration, used by `appservice generate-registration`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RegistrationSource {
    matrix: Option<RegistrationMatrix>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RegistrationMatrix {
    homeserver: Option<RegistrationHomeserver>,
    appservice: Option<RegistrationAppService>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RegistrationHomeserver {
    domain: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RegistrationAppService {
    id: Option<String>,
    url: Option<String>,
    sender_localpart: Option<String>,
}

impl RegistrationSource {
    fn load(config_path: Option<&str>) -> Result<Self> {
        let mut builder = ::config::Config::builder();
        if let Some(path) = resolve_config_path(config_path) {
            builder = builder.add_source(::config::File::from(path).required(true));
        }

        let source = builder
            .add_source(::config::Environment::with_prefix("CUMMENTS").separator("__"))
            .build()?;
        Ok(source.try_deserialize()?)
    }

    fn server_name(&self) -> Option<String> {
        self.matrix.as_ref()?.homeserver.as_ref()?.domain.clone()
    }

    fn appservice_id(&self) -> Option<String> {
        self.matrix.as_ref()?.appservice.as_ref()?.id.clone()
    }

    fn appservice_url(&self) -> Option<String> {
        self.matrix.as_ref()?.appservice.as_ref()?.url.clone()
    }

    fn sender_localpart(&self) -> Option<String> {
        self.matrix
            .as_ref()?
            .appservice
            .as_ref()?
            .sender_localpart
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cumments_core::models::{PostSlug, SiteId};
    use cumments_core::ports::RegistryStore;
    use cumments_core::site_auth::{OriginPattern, SiteVerificationPolicy};
    use cumments_store::DbStore;

    fn test_db_url(name: &str) -> String {
        let path = std::path::Path::new("/tmp").join(format!(
            "cumments-cli-test-{}-{}.db",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        format!("sqlite://{}", path.display())
    }

    fn test_policy() -> SiteAuthPolicy {
        SiteAuthPolicy {
            verification: SiteVerificationPolicy::Disabled,
            ..Default::default()
        }
    }

    #[test]
    fn registration_namespaces_use_underscored_prefixes() {
        let registration = build_registration(
            "http://localhost:3001",
            "a+b.example.com",
            "_cumments_bot",
            "cumments",
            false,
        );

        assert_eq!(registration.sender_localpart, "_cumments_bot");
        assert_eq!(registration.id, "cumments");
        assert_eq!(
            registration.namespaces.users[0].regex,
            "@_cumments_.*:a\\+b\\.example\\.com"
        );
        assert_eq!(
            registration.namespaces.aliases[0].regex,
            "#_cumments_.*:a\\+b\\.example\\.com"
        );
    }

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
            command: SitesCommand::ExportConfig(SiteIdArg {
                site_id: "my-blog".to_string(),
            }),
        };
        handle_sites_command(&store, &policy, &export)
            .await
            .expect("export config snippet");
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

    #[tokio::test]
    async fn rooms_list_blocked_and_unblock() {
        let store = DbStore::connect(&test_db_url("rooms"))
            .await
            .expect("connect db");
        let site = SiteId::from("my-blog");
        let slug = PostSlug::from("hello");
        store
            .register_room("!room:hs", &site, &slug)
            .await
            .expect("register room");
        store
            .mark_room_blocked("!room:hs", "Refusing to adopt room")
            .await
            .expect("mark blocked");

        let list = RoomsArgs {
            command: RoomsCommand::ListBlocked(BlockedListArgs {
                site_id: None,
                page: 1,
                per_page: 20,
                table: false,
            }),
        };
        handle_rooms_command(&store, &list)
            .await
            .expect("list blocked rooms");

        let unblock = RoomsArgs {
            command: RoomsCommand::Unblock(UnblockRoomArgs {
                room_id: "!room:hs".to_string(),
            }),
        };
        handle_rooms_command(&store, &unblock)
            .await
            .expect("unblock room");
        assert!(
            store
                .get_blocked_rooms()
                .await
                .expect("blocked rooms")
                .is_empty(),
            "room must no longer be blocked"
        );

        let missing = RoomsArgs {
            command: RoomsCommand::Unblock(UnblockRoomArgs {
                room_id: "!nope:hs".to_string(),
            }),
        };
        assert!(
            handle_rooms_command(&store, &missing).await.is_err(),
            "unknown room must fail"
        );
    }

    #[test]
    fn completions_command_tree_has_management_groups() {
        let command = crate::cli_command();
        let subcommands = command
            .get_subcommands()
            .map(|sub| sub.get_name().to_string())
            .collect::<Vec<_>>();
        for expected in [
            "appservice",
            "backfill",
            "backup",
            "sites",
            "rooms",
            "completions",
        ] {
            assert!(
                subcommands.iter().any(|name| name == expected),
                "missing subcommand `{expected}`"
            );
        }
    }
}

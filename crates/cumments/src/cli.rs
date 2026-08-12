//! CLI subcommands for Cumments.

use anyhow::Result;
use clap::Subcommand;
use cumments_core::models::SiteId;
use cumments_core::ports::SiteAuthStore;
use cumments_core::site_auth::{register_site, token_hash};
use rand::Rng;
use serde::{Deserialize, Serialize};
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
}

#[derive(clap::Args, Debug)]
pub struct RegisterSiteArgs {
    /// Optional explicit site id (operator-chosen). Without it, a random,
    /// unguessable id is generated.
    #[arg(long)]
    pub site_id: Option<String>,
}

/// All CLI subcommands.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Generate an AppService registration file
    #[command(name = "generate-registration")]
    GenerateRegistration(GenerateRegistrationArgs),
    /// Rebuild the read model from Matrix room history
    #[command(name = "backfill")]
    Backfill(BackfillArgs),
    /// Create a consistent single-file SQLite backup
    #[command(name = "backup")]
    Backup(BackupArgs),
    /// Manage sites registered through the API
    #[command(name = "sites")]
    Sites(SitesArgs),
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

/// Handles `cumments sites register`.
pub async fn handle_sites_command(store: &cumments_store::DbStore, args: &SitesArgs) -> Result<()> {
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
    }
}

/// Handle the `generate-registration` subcommand.
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

/// A partial view of the configuration, used by `generate-registration`.
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
}

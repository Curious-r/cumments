//! CLI subcommands for Cumments.

use anyhow::Result;
use clap::Subcommand;
use rand::Rng;
use serde::{Deserialize, Serialize};
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

    /// Rate-limited output (no as_token/hs_token values in stdout)
    #[arg(long, default_value_t = false)]
    pub quiet: bool,
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

    let registration = build_registration(&url, &server_name, &sender_localpart, &id, args.quiet);
    let yaml = serde_yaml::to_string(&registration)?;
    println!("{}", yaml);

    if !args.quiet {
        eprintln!("---");
        eprintln!("Add these tokens to your config.toml under [matrix.appservice]:");
        eprintln!("  as_token = \"{}\"", registration.as_token);
        eprintln!("  hs_token = \"{}\"", registration.hs_token);
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

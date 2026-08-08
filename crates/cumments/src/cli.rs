//! CLI subcommands for Cumments.

use anyhow::Result;
use clap::Subcommand;
use rand::RngCore;
use serde::Serialize;
use std::path::PathBuf;

/// Generate a complete registration.yaml for the AppService mode.
#[derive(clap::Args, Debug)]
pub struct GenerateRegistrationArgs {
    /// The URL where the homeserver can reach this Cumments instance
    #[arg(long, default_value = "http://localhost:3001")]
    pub url: String,

    /// The Matrix server name (domain)
    #[arg(long)]
    pub server_name: String,

    /// The localpart for the AppService's sender user
    #[arg(long, default_value = "cumments")]
    pub sender_localpart: String,

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
pub fn handle_generate_registration(args: &GenerateRegistrationArgs) -> Result<()> {
    let as_token = generate_token();
    let hs_token = generate_token();

    let registration = AppServiceRegistration {
        id: "cumments".to_string(),
        url: args.url.clone(),
        as_token: if args.quiet {
            "[REDACTED]".to_string()
        } else {
            as_token.clone()
        },
        hs_token: if args.quiet {
            "[REDACTED]".to_string()
        } else {
            hs_token.clone()
        },
        sender_localpart: args.sender_localpart.clone(),
        rate_limited: false,
        namespaces: Namespaces {
            users: vec![NamespaceRule {
                exclusive: true,
                regex: format!("@_cumments_.*:{}", regex_escape(&args.server_name)),
            }],
            aliases: vec![NamespaceRule {
                exclusive: true,
                regex: format!("#cumments_.*:{}", regex_escape(&args.server_name)),
            }],
            rooms: vec![],
        },
    };

    let yaml = serde_yaml::to_string(&registration)?;
    println!("{}", yaml);

    if !args.quiet {
        eprintln!("---");
        eprintln!("Add these tokens to your config.toml under [matrix]:");
        eprintln!("  as_token = \"{}\"", as_token);
        eprintln!("  hs_token = \"{}\"", hs_token);
    }

    Ok(())
}

/// Minimal regex escape for Matrix namespace patterns.
fn regex_escape(s: &str) -> String {
    // Only "." needs escaping for our limited use case
    s.replace('.', "\\.")
}

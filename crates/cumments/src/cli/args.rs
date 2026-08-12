//! Clap argument definitions and the completions command.

use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

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

/// Handles `cumments completions <shell>`.
pub fn handle_completions(args: &CompletionsArgs) -> Result<()> {
    let mut cmd = crate::cli_command();
    clap_complete::generate(args.shell, &mut cmd, "cumments", &mut std::io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
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

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

/// List the chat command audit log.
#[derive(clap::Args, Debug)]
pub struct AuditArgs {
    /// Only entries from this MXID.
    #[arg(long)]
    pub actor: Option<String>,
    /// Maximum entries to show.
    #[arg(long, default_value_t = 50)]
    pub limit: u64,
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
    ExportConfig(ExportConfigArgs),
    /// Rotate the claim token; the new token is printed exactly once
    #[command(name = "rotate-claim-token")]
    RotateClaimToken(SiteIdArg),
    /// Stop writes and retire the site's Matrix Space and rooms
    #[command(name = "retire")]
    Retire(RetireSiteArgs),
    /// Start a pending site-admin claim and print its one-time verify token
    #[command(name = "add-admin")]
    AddAdmin(SiteUserIdArg),
    /// Revoke a pending site-admin claim (applied roles are managed in Matrix)
    #[command(name = "remove-admin")]
    RemoveAdmin(SiteUserIdArg),
    /// Start a pending manager claim and print its one-time verify token
    #[command(name = "add-manager")]
    AddManager(SiteUserIdArg),
    /// Revoke a pending manager claim (applied roles are managed in Matrix)
    #[command(name = "remove-manager")]
    RemoveManager(SiteUserIdArg),
    /// Start an ownership transfer; the target must verify the claim token
    #[command(name = "transfer-owner")]
    TransferOwner(SiteUserIdArg),
}

#[derive(clap::Args, Debug)]
pub struct RegisterSiteArgs {
    /// Optional explicit site id (operator-chosen). Without it, a random,
    /// unguessable id is generated.
    #[arg(long)]
    pub site_id: Option<String>,
}

/// Arguments for listing sites (mirrors `QUERY /api/v1/operator/sites`).
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

/// A site id plus a target Matrix user id.
#[derive(clap::Args, Debug)]
pub struct SiteUserIdArg {
    pub site_id: String,
    pub user_id: String,
}

/// Arguments for exporting a config snippet.
#[derive(clap::Args, Debug)]
pub struct ExportConfigArgs {
    pub site_id: String,
    /// Print raw TOML instead of the JSON wrapper
    #[arg(long, default_value_t = false)]
    pub raw: bool,
}

/// Arguments for revoking the HMAC secret.
#[derive(clap::Args, Debug)]
pub struct RevokeSecretArgs {
    pub site_id: String,
    /// Confirm the destructive operation
    #[arg(long)]
    pub yes: bool,
}

/// Arguments for retiring a site.
#[derive(clap::Args, Debug)]
pub struct RetireSiteArgs {
    pub site_id: String,
    /// Confirm the destructive operation
    #[arg(long)]
    pub yes: bool,
    /// Poll until the background retirement finishes
    #[arg(long)]
    pub wait: bool,
}

/// Arguments for revoking a verified origin.
#[derive(clap::Args, Debug)]
pub struct RevokeOriginArgs {
    pub site_id: String,
    pub origin: String,
}

/// Quarantined room management subcommands.
#[derive(clap::Args, Debug)]
pub struct RoomsArgs {
    #[command(subcommand)]
    pub command: RoomsCommand,
}

/// Projection repair queue subcommands.
#[derive(clap::Args, Debug)]
pub struct ProjectionArgs {
    #[command(subcommand)]
    pub command: ProjectionCommand,
}

#[derive(Subcommand, Debug)]
pub enum ProjectionCommand {
    /// List durable Matrix facts awaiting projection repair
    #[command(name = "list-repairs")]
    ListRepairs(ListProjectionRepairsArgs),
}

/// Arguments for listing projection repairs.
#[derive(clap::Args, Debug)]
pub struct ListProjectionRepairsArgs {
    /// Filter by `pending`, `manual`, or `resolved`
    #[arg(long)]
    pub status: Option<String>,
    /// Maximum rows to show.
    #[arg(long, default_value_t = 50)]
    pub limit: u64,
}

#[derive(Subcommand, Debug)]
pub enum RoomsCommand {
    /// List rooms currently quarantined from adoption
    #[command(name = "list-quarantined")]
    ListQuarantined(QuarantinedListArgs),
    /// Clear a room's quarantine and make it canonical again
    #[command(name = "reinstate")]
    Reinstate(ReinstateRoomArgs),
    /// Upgrade a registered comment room through the homeserver's /upgrade
    #[command(name = "upgrade")]
    Upgrade(UpgradeRoomArgs),
    /// Stop writes and retire one comment room (leave Matrix, clear local
    /// projections in the background)
    #[command(name = "retire")]
    Retire(RetireRoomArgs),
}

/// Arguments for listing quarantined rooms (mirrors
/// `QUERY /api/v1/operator/quarantined-rooms`).
#[derive(clap::Args, Debug)]
pub struct QuarantinedListArgs {
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

/// Arguments for reinstating a room.
#[derive(clap::Args, Debug)]
pub struct ReinstateRoomArgs {
    pub room_id: String,
}

/// Arguments for retiring a comment room.
#[derive(clap::Args, Debug)]
pub struct RetireRoomArgs {
    pub room_id: String,
    /// Confirm the destructive operation
    #[arg(long)]
    pub yes: bool,
    /// Poll until the background retirement finishes
    #[arg(long)]
    pub wait: bool,
}

/// Arguments for upgrading a comment room.
#[derive(clap::Args, Debug)]
pub struct UpgradeRoomArgs {
    pub room_id: String,
    /// Target Matrix room version, e.g. `12`
    pub new_version: String,
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
    /// List the chat command audit log
    #[command(name = "audit")]
    Audit(AuditArgs),
    /// Manage sites registered through the API
    #[command(name = "sites")]
    Sites(SitesArgs),
    /// Manage quarantined comment rooms
    #[command(name = "rooms")]
    Rooms(RoomsArgs),
    /// Inspect the durable projection repair queue
    #[command(name = "projection")]
    Projection(ProjectionArgs),
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
            "projection",
            "completions",
        ] {
            assert!(
                subcommands.iter().any(|name| name == expected),
                "missing subcommand `{expected}`"
            );
        }
    }
}

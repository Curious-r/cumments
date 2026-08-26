//! Clap argument definitions for the stable CLI grammar.

use clap::Subcommand;
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct GenerateRegistrationArgs {
    #[arg(long)]
    pub url: Option<String>,
    #[arg(long)]
    pub server_name: Option<String>,
    #[arg(long)]
    pub sender_localpart: Option<String>,
    #[arg(long)]
    pub id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub quiet: bool,
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct BackfillArgs {
    #[arg(long, default_value_t = 500)]
    pub max_pages: u32,
}

#[derive(clap::Args, Debug)]
pub struct BackupCreateArgs {
    #[arg(short, long)]
    pub output: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum DatabaseCommand {
    #[command(name = "backups")]
    Backups(DatabaseBackupsArgs),
}

#[derive(clap::Args, Debug)]
pub struct DatabaseArgs {
    #[command(subcommand)]
    pub command: DatabaseCommand,
}

#[derive(clap::Args, Debug)]
pub struct DatabaseBackupsArgs {
    #[command(subcommand)]
    pub command: DatabaseBackupsCommand,
}

#[derive(Subcommand, Debug)]
pub enum DatabaseBackupsCommand {
    Create(BackupCreateArgs),
}

#[derive(Subcommand, Debug)]
pub enum AuditCommand {
    #[command(name = "entries")]
    Entries(AuditEntriesArgs),
}

#[derive(clap::Args, Debug)]
pub struct AuditArgs {
    #[command(subcommand)]
    pub command: AuditCommand,
}

#[derive(clap::Args, Debug)]
pub struct AuditEntriesArgs {
    #[command(subcommand)]
    pub command: AuditEntriesCommand,
}

#[derive(Subcommand, Debug)]
pub enum AuditEntriesCommand {
    List(AuditListArgs),
}

#[derive(clap::Args, Debug)]
pub struct AuditListArgs {
    #[arg(long)]
    pub actor: Option<String>,
    #[arg(long, default_value_t = 50)]
    pub limit: u64,
}

#[derive(clap::Args, Debug)]
pub struct SitesArgs {
    #[command(subcommand)]
    pub command: SitesCommand,
}

#[derive(Subcommand, Debug)]
pub enum SitesCommand {
    Register(RegisterSiteArgs),
    List(SiteListArgs),
    Get(SiteIdArg),
    ExportConfig(ExportConfigArgs),
    #[command(name = "origins")]
    Origins(SiteOriginsArgs),
    #[command(name = "secrets")]
    Secrets(SiteSecretsArgs),
    #[command(name = "claim-tokens")]
    ClaimTokens(SiteClaimTokensArgs),
    #[command(name = "admins")]
    Admins(SiteAdminsArgs),
    #[command(name = "managers")]
    Managers(SiteManagersArgs),
    #[command(name = "moderators")]
    Moderators(PageModeratorsArgs),
    #[command(name = "owners")]
    Owners(SiteOwnersArgs),
    #[command(name = "retirements")]
    Retirements(SiteRetirementsArgs),
    #[command(name = "packs")]
    Packs(SitePacksArgs),
}

#[derive(clap::Args, Debug)]
pub struct SiteOriginsArgs {
    #[command(subcommand)]
    pub command: SiteOriginsCommand,
}

#[derive(Subcommand, Debug)]
pub enum SiteOriginsCommand {
    Revoke(RevokeOriginArgs),
}

#[derive(clap::Args, Debug)]
pub struct SiteSecretsArgs {
    #[command(subcommand)]
    pub command: SiteSecretsCommand,
}

#[derive(Subcommand, Debug)]
pub enum SiteSecretsCommand {
    Rotate(SiteIdArg),
    Revoke(RevokeSecretArgs),
}

#[derive(clap::Args, Debug)]
pub struct SiteClaimTokensArgs {
    #[command(subcommand)]
    pub command: SiteClaimTokensCommand,
}

#[derive(Subcommand, Debug)]
pub enum SiteClaimTokensCommand {
    Rotate(SiteIdArg),
}

#[derive(clap::Args, Debug)]
pub struct SiteAdminsArgs {
    #[command(subcommand)]
    pub command: SiteAdminsCommand,
}

#[derive(Subcommand, Debug)]
pub enum SiteAdminsCommand {
    Add(SiteUserIdArg),
    Remove(SiteUserIdArg),
}

#[derive(clap::Args, Debug)]
pub struct SiteManagersArgs {
    #[command(subcommand)]
    pub command: SiteManagersCommand,
}

#[derive(Subcommand, Debug)]
pub enum SiteManagersCommand {
    Add(SiteUserIdArg),
    Remove(SiteUserIdArg),
}

#[derive(clap::Args, Debug)]
pub struct PageModeratorsArgs {
    #[command(subcommand)]
    pub command: PageModeratorsCommand,
}

#[derive(Subcommand, Debug)]
pub enum PageModeratorsCommand {
    Add(PageUserIdArg),
    Remove(PageUserIdArg),
}

#[derive(clap::Args, Debug)]
pub struct SiteOwnersArgs {
    #[command(subcommand)]
    pub command: SiteOwnersCommand,
}

#[derive(Subcommand, Debug)]
pub enum SiteOwnersCommand {
    Transfer(SiteUserIdArg),
}

#[derive(clap::Args, Debug)]
pub struct SiteRetirementsArgs {
    #[command(subcommand)]
    pub command: SiteRetirementsCommand,
}

#[derive(Subcommand, Debug)]
pub enum SiteRetirementsCommand {
    Create(RetireSiteArgs),
    Show(SiteIdArg),
}

#[derive(clap::Args, Debug)]
pub struct SitePacksArgs {
    #[command(subcommand)]
    pub command: SitePacksCommand,
}

#[derive(Subcommand, Debug)]
pub enum SitePacksCommand {
    #[command(name = "stickers")]
    Stickers(SitePackStickersArgs),
}

#[derive(clap::Args, Debug)]
pub struct SitePackStickersArgs {
    #[command(subcommand)]
    pub command: SitePackStickersCommand,
}

#[derive(Subcommand, Debug)]
pub enum SitePackStickersCommand {
    Add(AddStickerArgs),
    Remove(RemoveStickerArgs),
}

#[derive(clap::Args, Debug)]
pub struct RegisterSiteArgs {
    #[arg(long)]
    pub site_id: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct SiteListArgs {
    #[arg(long)]
    pub site_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i64,
    #[arg(long, default_value_t = 20)]
    pub per_page: i64,
    #[arg(long)]
    pub table: bool,
}

#[derive(clap::Args, Debug)]
pub struct SiteIdArg {
    pub site_id: String,
}

#[derive(clap::Args, Debug)]
pub struct SiteUserIdArg {
    pub site_id: String,
    pub user_id: String,
}

#[derive(clap::Args, Debug)]
pub struct PageUserIdArg {
    pub site_id: String,
    pub page_slug: String,
    pub user_id: String,
}

#[derive(clap::Args, Debug)]
pub struct AddStickerArgs {
    pub site_id: String,
    pub pack_id: String,
    pub shortcode: String,
    pub url: String,
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long)]
    pub info: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct RemoveStickerArgs {
    pub site_id: String,
    pub pack_id: String,
    pub shortcode: String,
}

#[derive(clap::Args, Debug)]
pub struct ExportConfigArgs {
    pub site_id: String,
    #[arg(long, default_value_t = false)]
    pub raw: bool,
}

#[derive(clap::Args, Debug)]
pub struct RevokeSecretArgs {
    pub site_id: String,
    #[arg(long)]
    pub yes: bool,
}

#[derive(clap::Args, Debug)]
pub struct RetireSiteArgs {
    pub site_id: String,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub confirm_site_id: Option<String>,
    #[arg(long)]
    pub wait: bool,
}

#[derive(clap::Args, Debug)]
pub struct RevokeOriginArgs {
    pub site_id: String,
    pub origin: String,
}

#[derive(clap::Args, Debug)]
pub struct PagesArgs {
    #[command(subcommand)]
    pub command: PagesCommand,
}

#[derive(Subcommand, Debug)]
pub enum PagesCommand {
    #[command(name = "upgrades")]
    Upgrades(PageUpgradesArgs),
    #[command(name = "retirements")]
    Retirements(PageRetirementsArgs),
}

#[derive(clap::Args, Debug)]
pub struct PageUpgradesArgs {
    #[command(subcommand)]
    pub command: PageUpgradesCommand,
}

#[derive(Subcommand, Debug)]
pub enum PageUpgradesCommand {
    Create(CreatePageUpgradeArgs),
}

#[derive(clap::Args, Debug)]
pub struct CreatePageUpgradeArgs {
    pub site_id: String,
    pub page_slug: String,
    pub new_version: String,
}

#[derive(clap::Args, Debug)]
pub struct PageRetirementsArgs {
    #[command(subcommand)]
    pub command: PageRetirementsCommand,
}

#[derive(Subcommand, Debug)]
pub enum PageRetirementsCommand {
    Create(RetirePageArgs),
    Show(PageIdArgs),
}

#[derive(clap::Args, Debug)]
pub struct RetirePageArgs {
    pub site_id: String,
    pub page_slug: String,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub wait: bool,
}

#[derive(clap::Args, Debug)]
pub struct PageIdArgs {
    pub site_id: String,
    pub page_slug: String,
}

#[derive(clap::Args, Debug)]
pub struct QuarantinedRoomsArgs {
    #[command(subcommand)]
    pub command: QuarantinedRoomsCommand,
}

#[derive(Subcommand, Debug)]
pub enum QuarantinedRoomsCommand {
    List(QuarantinedListArgs),
    Reinstate(ReinstateRoomArgs),
}

#[derive(clap::Args, Debug)]
pub struct QuarantinedListArgs {
    #[arg(long)]
    pub site_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i64,
    #[arg(long, default_value_t = 20)]
    pub per_page: i64,
    #[arg(long)]
    pub table: bool,
}

#[derive(clap::Args, Debug)]
pub struct ReinstateRoomArgs {
    pub room_id: String,
}

#[derive(clap::Args, Debug)]
pub struct RoomsArgs {
    #[command(subcommand)]
    pub command: RoomsCommand,
}

#[derive(Subcommand, Debug)]
pub enum RoomsCommand {
    #[command(name = "upgrades")]
    Upgrades(RoomUpgradesArgs),
    #[command(name = "retirements")]
    Retirements(RoomRetirementsArgs),
}

#[derive(clap::Args, Debug)]
pub struct RoomUpgradesArgs {
    #[command(subcommand)]
    pub command: RoomUpgradesCommand,
}

#[derive(Subcommand, Debug)]
pub enum RoomUpgradesCommand {
    Create(UpgradeRoomArgs),
}

#[derive(clap::Args, Debug)]
pub struct UpgradeRoomArgs {
    pub room_id: String,
    pub new_version: String,
}

#[derive(clap::Args, Debug)]
pub struct RoomRetirementsArgs {
    #[command(subcommand)]
    pub command: RoomRetirementsCommand,
}

#[derive(Subcommand, Debug)]
pub enum RoomRetirementsCommand {
    Create(RetireRoomArgs),
    Show(RoomIdArg),
}

#[derive(clap::Args, Debug)]
pub struct RoomIdArg {
    pub room_id: String,
}

#[derive(clap::Args, Debug)]
pub struct RetireRoomArgs {
    pub room_id: String,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub wait: bool,
}

#[derive(clap::Args, Debug)]
pub struct ProjectionRepairsArgs {
    #[command(subcommand)]
    pub command: ProjectionRepairsCommand,
}

#[derive(Subcommand, Debug)]
pub enum ProjectionRepairsCommand {
    List(ListProjectionRepairsArgs),
    Get(EventIdArg),
    Retry(EventIdArg),
}

#[derive(clap::Args, Debug)]
pub struct EventIdArg {
    pub target_event_id: String,
}

#[derive(clap::Args, Debug)]
pub struct ListProjectionRepairsArgs {
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: i64,
    #[arg(long, default_value_t = 50)]
    pub per_page: i64,
    #[arg(long)]
    pub table: bool,
}

#[derive(clap::Args, Debug)]
pub struct AppserviceArgs {
    #[command(subcommand)]
    pub command: AppserviceCommand,
}

#[derive(Subcommand, Debug)]
pub enum AppserviceCommand {
    #[command(name = "registrations")]
    Registrations(AppserviceRegistrationsArgs),
}

#[derive(clap::Args, Debug)]
pub struct AppserviceRegistrationsArgs {
    #[command(subcommand)]
    pub command: AppserviceRegistrationsCommand,
}

#[derive(Subcommand, Debug)]
pub enum AppserviceRegistrationsCommand {
    Generate(GenerateRegistrationArgs),
}

#[derive(clap::Args, Debug)]
pub struct CompletionsArgs {
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    Appservice(AppserviceArgs),
    Backfill(BackfillArgs),
    Database(DatabaseArgs),
    Audit(AuditArgs),
    Sites(SitesArgs),
    Pages(PagesArgs),
    #[command(name = "quarantined-rooms")]
    QuarantinedRooms(QuarantinedRoomsArgs),
    Rooms(RoomsArgs),
    #[command(name = "projection-repairs")]
    ProjectionRepairs(ProjectionRepairsArgs),
    Completions(CompletionsArgs),
}

pub fn handle_completions(args: &CompletionsArgs) -> anyhow::Result<()> {
    let mut cmd = crate::cli_command();
    clap_complete::generate(args.shell, &mut cmd, "cumments", &mut std::io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    fn find<'a>(command: &'a clap::Command, path: &[&str]) -> Option<&'a clap::Command> {
        let mut current = command;
        for name in path {
            current = current.find_subcommand(*name)?;
        }
        Some(current)
    }

    #[test]
    fn cli_uses_domain_resource_verb_tree() {
        let root = crate::cli_command();
        assert!(find(&root, &["database", "backups", "create"]).is_some());
        assert!(find(&root, &["audit", "entries", "list"]).is_some());
        assert!(find(&root, &["appservice", "registrations", "generate"]).is_some());
        assert!(find(&root, &["sites", "secrets", "rotate"]).is_some());
        assert!(find(&root, &["sites", "admins", "add"]).is_some());
        assert!(find(&root, &["sites", "moderators", "remove"]).is_some());
        assert!(find(&root, &["pages", "upgrades", "create"]).is_some());
        assert!(find(&root, &["quarantined-rooms", "reinstate"]).is_some());
        assert!(find(&root, &["rooms", "retirements", "show"]).is_some());
        assert!(find(&root, &["projection-repairs", "retry"]).is_some());
    }
}

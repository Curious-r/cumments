//! Stable registry for Matrix bot commands.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmTier {
    None,
    Explicit,
    ConfirmFlag,
}

pub struct BotCommand {
    pub id: &'static str,
    pub syntax: &'static str,
    pub summary: &'static str,
    pub confirm: ConfirmTier,
}

const fn command(
    id: &'static str,
    syntax: &'static str,
    summary: &'static str,
    confirm: ConfirmTier,
) -> BotCommand {
    BotCommand {
        id,
        syntax,
        summary,
        confirm,
    }
}

/// This is the command contract. Execution branches must cover exactly these
/// resources; admin and ownership transfer are deliberately absent because
/// site-owner bootstrap remains the authority for those capabilities.
pub const BOT_COMMANDS: &[BotCommand] = &[
    command("help", "help", "Show command help.", ConfirmTier::None),
    command(
        "sites.list",
        "sites list",
        "List sites (instance operator).",
        ConfirmTier::None,
    ),
    command(
        "sites.register",
        "sites register <site_id>",
        "Register a site and become its first admin.",
        ConfirmTier::None,
    ),
    command(
        "sites.use",
        "sites use <site_id>",
        "Set your active site.",
        ConfirmTier::None,
    ),
    command(
        "sites.status",
        "sites status [site_id]",
        "Show site status.",
        ConfirmTier::None,
    ),
    command(
        "managers.add",
        "managers add <site_id> <mxid>",
        "Start a manager claim.",
        ConfirmTier::Explicit,
    ),
    command(
        "managers.remove",
        "managers remove <site_id> <mxid> --confirm",
        "Remove a pending or applied manager.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "managers.resign",
        "managers resign <site_id> --confirm",
        "Resign as manager.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "moderators.add",
        "moderators add <site_id> <page_slug> <mxid>",
        "Start a moderator claim.",
        ConfirmTier::Explicit,
    ),
    command(
        "moderators.remove",
        "moderators remove <site_id> <page_slug> <mxid> --confirm",
        "Remove a pending or applied moderator.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "moderators.resign",
        "moderators resign <site_id> <page_slug> --confirm",
        "Resign as moderator.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "upgrades.create",
        "pages upgrades create <site_id> <page_slug> <version> --confirm",
        "Upgrade a comment room.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "retirements.create.page",
        "pages retirements create <site_id> <page_slug> --confirm",
        "Retire a page comment room.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "stickers.list",
        "stickers list <site_id>",
        "List sticker packs.",
        ConfirmTier::None,
    ),
    command(
        "stickers.add",
        "stickers add <site_id> <pack_id> <shortcode> <mxc> [body...]",
        "Add or replace a sticker.",
        ConfirmTier::Explicit,
    ),
    command(
        "stickers.remove",
        "stickers remove <site_id> <pack_id> <shortcode> --confirm",
        "Remove a sticker.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "secrets.issue",
        "secrets issue <site_id>",
        "Issue an HMAC secret once.",
        ConfirmTier::Explicit,
    ),
    command(
        "claim-tokens.rotate",
        "claim-tokens rotate <site_id>",
        "Rotate a claim token (instance operator).",
        ConfirmTier::Explicit,
    ),
    command(
        "retirements.create.site",
        "retirements create <site_id> --confirm",
        "Retire a site.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "quarantined-rooms.list",
        "quarantined-rooms list",
        "List quarantined rooms (instance operator).",
        ConfirmTier::None,
    ),
    command(
        "quarantined-rooms.reinstate",
        "quarantined-rooms reinstate <room_id> --confirm",
        "Reinstate a quarantined room.",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "rooms.upgrades.create",
        "rooms upgrades create <room_id> <version> --confirm",
        "Upgrade a registered room (instance operator).",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "rooms.retirements.create",
        "rooms retirements create <room_id> --confirm",
        "Retire a registered room (instance operator).",
        ConfirmTier::ConfirmFlag,
    ),
    command(
        "backfill.start",
        "backfill [max_pages]",
        "Queue read-model backfill (instance operator).",
        ConfirmTier::None,
    ),
];

pub fn help_text() -> String {
    let mut help = String::from("Cumments bot 命令：\n");
    for command in BOT_COMMANDS {
        let confirm = match command.confirm {
            ConfirmTier::None => "",
            ConfirmTier::Explicit => "",
            ConfirmTier::ConfirmFlag => "（需要 --confirm）",
        };
        help.push_str(&format!(
            "!cumments {} — {}{}\n",
            command.syntax, command.summary, confirm
        ));
    }
    help.push_str("敏感 token 只在本私聊显示。admin/ownership 命令刻意不开放。");
    help
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn command_ids_and_syntaxes_are_unique() {
        let ids: HashSet<_> = BOT_COMMANDS.iter().map(|command| command.id).collect();
        let syntaxes: HashSet<_> = BOT_COMMANDS.iter().map(|command| command.syntax).collect();
        assert_eq!(ids.len(), BOT_COMMANDS.len());
        assert_eq!(syntaxes.len(), BOT_COMMANDS.len());
    }

    #[test]
    fn generated_help_covers_the_registry() {
        let help = help_text();
        assert!(help.contains("!cumments sites register <site_id>"));
        assert!(help.contains("--confirm"));
        assert!(help.contains("admin/ownership"));
    }
}

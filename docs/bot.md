# Matrix bot reference

Bot commands are accepted only in a verified private channel with exactly the
sender and the Cumments bot. A command elsewhere is consumed silently so it is
never projected as a comment. Sensitive tokens are printed only in that private
channel.

The private channel for Bot commands and `cumments-claim:...` tokens must be
**unencrypted** — the Bot currently does not decrypt `m.room.encrypted` and
therefore cannot read `!cumments ...` or claim tokens in an encrypted room
(it will join but appear unresponsive). If the homeserver or client creates
E2EE DMs by default (notably Tuwunel
`encryption_enabled_by_default_for_room_type = "invite"`), create the DM with
encryption disabled, re-invite the bot and resend the command. This is a
Bot-command limitation, not a Matrix rule; comment rooms may be encrypted or
not, and site owners may be on a different homeserver (federated) — only the
Bot DM for commands must be unencrypted.

Commands use `<domain> <resource> <verb>` syntax. Commands marked **confirm**
must be repeated with `--confirm`; the first invocation returns the exact
confirmation command.

## Site lifecycle and status

| Command | Permission | Effect |
| --- | --- | --- |
| `!cumments sites register <site_id>` | Public self-service | Register a site; the sender becomes its first admin and receives a one-time claim token. |
| `!cumments sites list` | Instance operator | List database and configuration-declared sites. |
| `!cumments sites use <site_id>` | Site admin | Set your active site for shorthand status. |
| `!cumments sites status` / `!cumments sites status <site_id>` | Site admin | Show site lifecycle, Space, and room summary. |
| `!cumments retirements create <site_id> --confirm` | Site admin | Mark the site retiring; reconciliation continues in the background. |

## Roles

Role claims are activated by the target user sending
`cumments-claim:<verify_token>` to the bot.

| Command | Permission | Effect |
| --- | --- | --- |
| `!cumments managers add <site_id> <mxid>` | Site admin | Create a pending manager claim. |
| `!cumments managers remove <site_id> <mxid> --confirm` | Site admin | Cancel a claim or remove an applied manager. |
| `!cumments managers resign <site_id> --confirm` | Manager | Resign from site manager. |
| `!cumments moderators add <site_id> <page_slug> <mxid>` | Room power >= 75 | Create a pending moderator claim. |
| `!cumments moderators remove <site_id> <page_slug> <mxid> --confirm` | Room power >= 75 | Cancel a claim or remove an applied moderator. |
| `!cumments moderators resign <site_id> <page_slug> --confirm` | Moderator | Resign from page moderator. |

Admin add/remove and ownership-transfer bot commands are deliberately absent.
Site-owner bootstrap is the trust boundary for those capabilities, preventing a
chat actor from rewriting ownership or the admin roster.

## Pages and stickers

| Command | Permission | Effect |
| --- | --- | --- |
| `!cumments pages upgrades create <site_id> <page_slug> <version> --confirm` | Site admin | Upgrade a registered comment room through the homeserver. |
| `!cumments pages retirements create <site_id> <page_slug> --confirm` | Site admin | Retire one page comment room. |
| `!cumments stickers list <site_id>` | Site admin or manager | List sticker packs and images. |
| `!cumments stickers add <site_id> <pack_id> <shortcode> <mxc> [body...]` | Site admin or manager | Add or replace an image in a pack. |
| `!cumments stickers remove <site_id> <pack_id> <shortcode> --confirm` | Site admin or manager | Remove an image from a pack. |

## Operator operations

| Command | Permission | Effect |
| --- | --- | --- |
| `!cumments quarantined-rooms list` | Instance operator | List rooms blocked by failed adoption checks. |
| `!cumments quarantined-rooms reinstate <room_id> --confirm` | Instance operator | Clear quarantine and make the room canonical again. |
| `!cumments rooms upgrades create <room_id> <version> --confirm` | Instance operator | Upgrade a raw registered room ID. |
| `!cumments rooms retirements create <room_id> --confirm` | Instance operator | Retire a raw registered room ID. |
| `!cumments claim-tokens rotate <site_id>` | Instance operator | Rotate a site claim token; shown once. |
| `!cumments secrets issue <site_id>` | Site admin | Issue an HMAC secret; shown once. |
| `!cumments backfill [max_pages]` | Instance operator | Queue one backfill job (default 500 pages). |

`backfill` requires a running worker in the same process. If no queue is wired,
the bot reports that it is unavailable rather than silently doing nothing.

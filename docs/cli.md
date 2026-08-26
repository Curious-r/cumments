# Command line interface

The `cumments` binary doubles as a local administration CLI. It operates
directly on the same SQLite database the server uses, so **whoever can read
and write the database is an operator** — there is no separate operator
token on the CLI path. Use the Operator HTTP API when managing a remote
instance.

The CLI mirrors the Operator API: anything the Operator API can do, the CLI can do
locally.

## Global flags

- `--config <path>` (also `-c`): configuration file. The flag is **global**,
  so it can appear before or after a subcommand:

  ```bash
  cumments --config cumments.toml sites list
  cumments sites list --config cumments.toml
  ```

- `--help`, `--version`.

## Command tree

```text
cumments
├── appservice
│   └── registrations
│       └── generate [--url ...] [--server-name ...] [--id ID]
│                    [--sender-localpart LOCALPART] [--quiet] [--output FILE]
├── backfill [--max-pages N]
├── database
│   └── backups
│       └── create --output FILE
├── audit
│   └── entries
│       └── list [--actor MXID] [--limit N]
├── sites
│   ├── register [--site-id ID]
│   ├── list [--site-id ID] [--page N] [--per-page N] [--table]
│   ├── get SITE_ID
│   ├── export-config [--raw] SITE_ID
│   ├── origins
│   │   └── revoke SITE_ID ORIGIN
│   ├── secrets
│   │   ├── rotate SITE_ID
│   │   └── revoke SITE_ID --yes
│   ├── claim-tokens
│   │   └── rotate SITE_ID
│   ├── admins
│   │   ├── add SITE_ID USER_ID
│   │   └── remove SITE_ID USER_ID
│   ├── managers
│   │   ├── add SITE_ID USER_ID
│   │   └── remove SITE_ID USER_ID
│   ├── moderators
│   │   ├── add SITE_ID PAGE_SLUG USER_ID
│   │   └── remove SITE_ID PAGE_SLUG USER_ID
│   ├── owners
│   │   └── transfer SITE_ID USER_ID
│   ├── retirements
│   │   ├── create SITE_ID --yes --confirm-site-id SITE_ID [--wait]
│   │   └── show SITE_ID
│   └── packs
│       └── stickers
│           ├── add SITE_ID PACK_ID SHORTCODE URL [--body TEXT] [--info JSON]
│           └── remove SITE_ID PACK_ID SHORTCODE
├── pages
│   ├── upgrades
│   │   └── create SITE_ID PAGE_SLUG VERSION
│   └── retirements
│       ├── create SITE_ID PAGE_SLUG --yes [--wait]
│       └── show SITE_ID PAGE_SLUG
├── quarantined-rooms
│   ├── list [--site-id ID] [--page N] [--per-page N] [--table]
│   └── reinstate ROOM_ID
├── rooms
│   ├── upgrades
│   │   └── create ROOM_ID VERSION
│   └── retirements
│       ├── create ROOM_ID --yes [--wait]
│       └── show ROOM_ID
├── projection-repairs
│   ├── list [--status pending|manual|resolved] [--page N] [--per-page N] [--table]
│   ├── get TARGET_EVENT_ID
│   └── retry TARGET_EVENT_ID
└── completions SHELL
```

## Output conventions

- Machine-readable data (site lists, quarantined rooms, secrets, tokens) goes to
  **stdout as JSON**, matching the Operator API response shape. `--table` on
  list commands switches to a human-readable table.
- Human notes and warnings go to **stderr**, so stdout stays script-friendly.
- Secrets and claim tokens are printed **exactly once**; the CLI refuses to
  show them again.
- `export-config` prints the `{"site_id","toml"}` wrapper returned by the
  Operator API; `--raw` prints only the TOML block for shell redirection.
- `rooms reinstate` prints `{"room_id","status":"active"}`. This is a CLI-side
  enhancement of the Operator API, which returns an empty `204`: the CLI has no
  body-free "no content" convention, so it reports the affected resource.
- `rooms upgrade` prints `{"room_id","new_version","replacement_room"}`,
  matching the Operator API response.
- Retirement and repair transitions print the affected resource plus its
  accepted state (for example `"retiring"` or `"pending"`); `--wait` replaces
  that state with the completed state when it succeeds within five minutes.

Paginated reads use the stable `{ "data": [...], "meta": { "total", "page",
"per_page", "total_pages" } }` envelope.

## Exit codes

| Code | Meaning |
|---:|---|
| 0 | Success, including an idempotent no-op. |
| 1 | Unclassified runtime error. |
| 2 | Usage error from Clap. |
| 10 | Input validation failed. |
| 11 | Resource not found. |
| 12 | Conflict or wrong resource state. |
| 13 | Authorization denied. |
| 14 | Database, Matrix, or other dependency unavailable. |
| 15 | Required confirmation missing. |

## Examples

List managed sites (database rows merged with the `[sites]` overlay):

```bash
cumments sites list
cumments sites list --site-id my-blog --table
```

List quarantined rooms and reinstate one:

```bash
cumments quarantined-rooms list
cumments quarantined-rooms reinstate '!ps4zwsSTsR6qph4L8Yqi5j6wfALV1-EIY5cI1TCq8DE'
```

Inspect durable projection repairs. Rows in `manual` need operator attention;
successful repairs move to `resolved`:

```bash
cumments projection-repairs list --status manual
cumments projection-repairs retry '$target:hs'
```

Upgrade a comment room (the target version must be newer than the room's
current version, e.g. upgrading a v11 room to 12):

```bash
cumments rooms upgrades create '!ps4zwsSTsR6qph4L8Yqi5j6wfALV1-EIY5cI1TCq8DE' 12
```

List the chat command audit log (newest first), optionally filtered by actor:

```bash
cumments audit entries list
cumments audit entries list --actor '@alice:example.com' --limit 20
```

Rotate a site's HMAC secret (printed once) or revoke it (destructive, needs
`--yes`):

```bash
cumments sites secrets rotate my-blog
cumments sites secrets revoke my-blog --yes
```

Export a TOML block to move a database-tracked site into declarative
configuration:

```bash
cumments sites export-config my-blog
cumments sites export-config --raw my-blog >> cumments.toml
```

cumments sites packs stickers add my-blog default cat 'mxc://server/cat' \
  --body 'A cat' --info '{"w":10}'
cumments sites packs stickers remove my-blog default cat

Register or revoke a role. Role `add-*` commands store a pending claim and
print the one-time `verify_token`; the target Matrix account must DM
`cumments-claim:<token>` to the AS bot before the role is applied. Removal
uses the same local management use case as the API:

```bash
cumments sites admins add my-blog '@alice:example.com'
cumments sites admins remove my-blog '@alice:example.com'
cumments sites managers add my-blog '@bob:example.com'
cumments sites owners transfer my-blog '@carol:example.com'
```

`remove-*` cancels a pending claim; a role that has already been applied is
removed from Matrix power levels directly. In `matrix.mode = "logging"` there
is no real homeserver, so the local claim row is updated but Matrix state is
not actually changed.

Retire a site (severe; requires both flags). The command marks the site
`retiring` — writes stop immediately — and the **running server's**
background reconciler retires its Matrix Space and rooms, then clears
the local data. Without `--wait` the command returns once the site is marked;
with `--wait` it polls until the retirement finishes (or times out after
five minutes). Config-declared sites cannot be retired; remove them from the
config file instead.

```bash
cumments sites retirements create my-blog --yes --confirm-site-id my-blog
cumments sites retirements create my-blog --yes --confirm-site-id my-blog --wait
```

## Shell completions

Generate a completion script and source it from your shell profile:

```bash
cumments completions bash   # or zsh / fish / powershell
```

For example, with bash:

```bash
cumments completions bash > ~/.local/share/bash-completion/completions/cumments
```

## Safety

- Credential rotation is explicit but does not require a confirmation flag;
  secret revocation requires `--yes`. Whole-site retirement also requires the
  site id to be repeated with `--confirm-site-id`.
- Page and room retirement require `--yes`.
- `database backups create --output` refuses to overwrite an existing file.
- Origins and secrets declared in `[sites]` cannot be changed through the CLI
  (or the Operator API): edit the configuration file instead.

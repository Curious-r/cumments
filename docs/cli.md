# Command line interface

The `cumments` binary doubles as a local administration CLI. It operates
directly on the same SQLite database the server uses, so **whoever can read
and write the database is an administrator** — there is no separate admin
token on the CLI path. Use the admin HTTP API when managing a remote
instance.

The CLI mirrors the admin API: anything the admin API can do, the CLI can do
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
├── generate-registration [--url ...] [--server-name ...] [--output FILE]
├── backfill [--max-pages N]
├── backup --output FILE
├── sites
│   ├── register [--site-id ID]
│   ├── list [--site-id ID] [--page N] [--per-page N] [--table]
│   ├── revoke-origin SITE_ID ORIGIN
│   ├── rotate-secret SITE_ID
│   ├── revoke-secret SITE_ID --yes
│   ├── export-config SITE_ID
│   └── rotate-claim-token SITE_ID
├── rooms
│   ├── list-blocked [--site-id ID] [--page N] [--per-page N] [--table]
│   └── unblock ROOM_ID
└── completions SHELL
```

## Output conventions

- Machine-readable data (site lists, blocked rooms, secrets, tokens) goes to
  **stdout as JSON**, matching the admin API response shape. `--table` on
  list commands switches to a human-readable table.
- Human notes and warnings go to **stderr**, so stdout stays script-friendly.
- Secrets and claim tokens are printed **exactly once**; the CLI refuses to
  show them again.
- Exit codes: `0` success, `1` runtime error, `2` usage error (clap).

## Examples

List managed sites (database rows merged with the `[sites]` overlay):

```bash
cumments sites list
cumments sites list --site-id my-blog --table
```

List and clear blocked rooms:

```bash
cumments rooms list-blocked
cumments rooms unblock '!ps4zwsSTsR6qph4L8Yqi5j6wfALV1-EIY5cI1TCq8DE'
```

Rotate a site's HMAC secret (printed once) or revoke it (destructive, needs
`--yes`):

```bash
cumments sites rotate-secret my-blog
cumments sites revoke-secret my-blog --yes
```

Export a TOML block to move a database-tracked site into declarative
configuration:

```bash
cumments sites export-config my-blog >> cumments.toml
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

- `sites revoke-secret` requires `--yes` because it removes a credential and
  changes the site's write path.
- `backup --output` refuses to overwrite an existing file.
- Origins and secrets declared in `[sites]` cannot be changed through the CLI
  (or the admin API): edit the configuration file instead.

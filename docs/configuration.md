# Configuration

## File discovery

The configuration file is discovered in this order:

1. `--config <path>`
2. `CUMMENTS_CONFIG` environment variable
3. `$XDG_CONFIG_HOME/cumments/cumments.toml` (or `~/.config/cumments/cumments.toml`)
4. `/etc/cumments/cumments.toml`
5. `./cumments.toml` (local development fallback)

Once a file is selected, effective value precedence is:

1. Environment variables (`CUMMENTS__` prefix, `__` level separator)
2. Values from the discovered config file
3. Built-in defaults

## Example AppService configuration

```toml
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"

[database]
url = "sqlite://data/cumments.db"

[security]
# Replace with a random secret; this is a literal value, not a ${VAR} expansion.
pow_secret = "pow_secret_key"
pow_difficulty = 4

[matrix]
mode = "appservice"

[matrix.homeserver]
# CS API base URL used by the AppService to call the homeserver
address = "https://matrix.example.com"
# Matrix ID domain (the part after ':' in user IDs and aliases)
domain = "example.com"

[matrix.appservice]
# Must match the `id` in registration.yaml
id = "cumments"
# URL the homeserver uses to reach this instance (must be reachable from the HS)
url = "https://cumments.example.com"
listen_host = "0.0.0.0"
listen_port = 3001
sender_localpart = "_cumments_bot"
# Tokens must match registration.yaml; prefer environment variables:
# CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN=...
# CUMMENTS__MATRIX__APPSERVICE__HS_TOKEN=...
# as_token = "<as_token from registration.yaml>"
# hs_token = "<hs_token from registration.yaml>"
# Optional: verify at startup that this config matches registration.yaml
# registration_file = "registration.yaml"
# Optional: Matrix room version to request when creating rooms (e.g. "12").
# When unset, the homeserver's configured default is used.
# room_version = "12"

[matrix.moderation]
owner_id = "@admin:your_server.tld"
```

For local development, set `mode = "logging"`; no `matrix.homeserver`,
`matrix.appservice`, or `matrix.moderation` sections are needed.

## Notes

- `matrix.homeserver.address` is the CS API endpoint used by the AppService.
  `matrix.homeserver.domain` is the Matrix ID domain; they are deliberately
  separate because reverse proxies and well-known delegation make them diverge.
- `matrix.appservice.url` is the callback URL the homeserver uses to reach
  Cumments, so it must be reachable *from* the homeserver, not from your
  browser.
- Environment variables are spelled with the `CUMMENTS__` prefix and `__`
  separators, e.g. `CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN`. The whole
  configuration can come from environment variables alone (the bundled
  `misc/docker/compose.yaml` does exactly this); a config file is optional,
  and `--config <path>` overrides file discovery.
- The schema is strict: unknown keys are rejected, so old flat field names
  (`matrix.homeserver_url`, `matrix.server_name`, ...) fail fast instead of
  being silently ignored.
- `cors_origins` is enforced: `"*"` keeps permissive CORS, a comma-separated
  list restricts `Access-Control-Allow-Origin` to those exact origins, and an
  empty value sends no CORS headers.
- SQLite files are created automatically, but the parent directory must exist
  (the repo has a `data/` directory).
- All timestamps are stored in UTC with millisecond precision.

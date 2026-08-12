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

[database]
url = "sqlite://data/cumments.db"

[security]
# Replace with a random secret; this is a literal value, not a ${VAR} expansion.
pow_secret = "pow_secret_key"
pow_difficulty = 4
# "disabled" | "optional" | "required"
site_verification = "optional"
# Operator token for the admin API (optional, at least 32 chars); unset
# disables the admin routes. Prefer the environment variable:
# CUMMENTS__SECURITY__ADMIN_TOKEN=...

[sites."my-blog"]
# "origin" (browser Origin, default) or "secret" (HMAC via edge function)
auth_mode = "origin"
# Exact origins or `https://*.example.com` wildcards, trusted without proof
allowed_origins = ["https://blog.example.com"]

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
admin_id = "@admin:your_server.tld"
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
- `server.cors_origins` has been **removed**: CORS headers are now derived
  from the site registry (see below). A config file that still contains the
  key fails startup because unknown keys are rejected.
- `security.admin_token` enables the admin API (see [api.md](api.md)).
  Placeholder values and tokens shorter than 32 characters are rejected at
  startup.
- SQLite files are created automatically, but the parent directory must exist
  (the repo has a `data/` directory).
- All timestamps are stored in UTC with millisecond precision.
- `server.trusted_proxies` lists reverse proxies allowed to set
  `X-Forwarded-For`; rate limiting ignores the header from any other peer.
- `security.allow_private_verification_origins` (default `false`) permits
  verification of loopback/private/link-local IP-literal origins; keep it
  disabled in production because `confirm` makes outbound HTTP/DNS probes.

## Site verification and write-path authentication

`security.site_verification` controls the instance-wide policy:

| Value | Unverified sites | Verified / configured sites |
|---|---|---|
| `disabled` | writes allowed, no checks (local development) | no checks |
| `optional` (default) | writes allowed, WARN log (migration) | enforced |
| `required` | writes rejected | enforced |

Per-site trust comes from two sources whose union is the effective rule set:

- **`[sites."<site_id>"]`** in this file: operator-declared trust. `auth_mode`
  is `"origin"` (default) or `"secret"`; `allowed_origins` accepts exact
  origins and `https://*.example.com` subdomain wildcards; `auth_mode =
  "secret"` requires a `secret` (HMAC key, at least 32 chars) that is best
  injected through `CUMMENTS__SITES__<site_id>__SECRET`.
- **Self-service registration** through `POST /api/v1/sites`: returns a
  random `site_id` and a one-time claim token. The owner proves domain
  control via `/.well-known/cumments.json` or a DNS TXT record, then switches
  to secret auth via the secret endpoint. See [API](api.md).

Origin-mode requests are accepted only when the browser `Origin` matches the
effective allowlist; `Origin: null` and missing `Origin` are rejected. The
dev-only `disabled` policy is the exception: it accepts every origin,
including `Origin: null` from `file://` demo pages, and answers with
`Access-Control-Allow-Origin: *`. Secret-mode requests must carry
`X-Cumments-Timestamp` and `X-Cumments-Signature` (HMAC-SHA256 over
`timestamp\nMETHOD\npath\nsha256(body)`), with the timestamp within ±5
minutes.

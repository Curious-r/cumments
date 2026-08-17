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
# public_base_url = "https://comments.example.net"

[database]
url = "sqlite://data/cumments.db"

[security]
# Replace with a random secret; this is a literal value, not a ${VAR} expansion.
pow_secret = "pow_secret_key"
pow_difficulty = 4
# "disabled" | "optional" | "required"
site_verification = "optional"
# Operator token for the Operator API (optional, at least 32 chars); unset
# disables the operator routes. Prefer the environment variable:
# CUMMENTS__SECURITY__OPERATOR_TOKEN=...
# Independent HMAC key for signed media-proxy URLs. When unset, the
# AppService token is used and rotating it invalidates outstanding media
# URLs; set a stable random value for production.
# media_sign_key = "change-me"
# Allow the media proxy to fetch loopback/private/link-local server names.
# Keep false in production: the proxy is an SSRF surface.
# media_proxy_allow_private_servers = false

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
# Support is checked best-effort before creating a room; if the homeserver
# cannot confirm (or rejects the check), the homeserver itself decides.
# room_version = "12"
```

For local development, set `mode = "logging"`; no `matrix.homeserver` or
`matrix.appservice` section is needed.

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
- CORS headers are derived from the site registry (see below); the legacy
  `server.cors_origins` key is not part of the schema and fails startup like
  any unknown key.
- `security.operator_token` enables the Operator API (see [API](api/index.md)).
  Placeholder values and tokens shorter than 32 characters are rejected at
  startup.
- SQLite files are created automatically, but the parent directory must exist
  (the repo has a `data/` directory).
- All timestamps are stored in UTC with millisecond precision.
- `server.trusted_proxies` declares which reverse proxies may set
  `X-Forwarded-For`; see [Reverse proxy trust](#reverse-proxy-trust) below.
  When overriding through the environment, use a comma-separated string
  (`CUMMENTS__SERVER__TRUSTED_PROXIES="loopback,10.0.0.0/8"`) or a
  JSON-style array string (`["loopback", "10.0.0.0/8"]`).
- `server.public_base_url` (optional) is the externally reachable base URL of
  this API (for example `https://comments.example.net`). Media proxy URLs are
  minted as absolute URLs against it, which is required when comment sections
  are embedded on other origins. When unset, the base is derived from each
  request instead: the `Host` header, plus `X-Forwarded-Proto` /
  `X-Forwarded-Host` when the peer is in `server.trusted_proxies`. Set the
  explicit value when forwarded headers are unreliable or you want a fixed
  public origin regardless of how clients reach the API.
- `security.allow_private_verification_origins` (default `false`) permits
  verification of loopback/private/link-local IP-literal origins; keep it
  disabled in production because `confirm` makes outbound HTTP/DNS probes.

## Reverse proxy trust

`server.trusted_proxies` accepts an array of named presets and CIDR
networks:

```toml
[server]
trusted_proxies = ["loopback", "private", "10.42.0.0/16"]
```

Each entry is either a preset or a CIDR:

| 条目 | 展开 / 含义 |
|---|---|
| `loopback` | `127.0.0.0/8`、`::1/128` |
| `private` | `10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`、`fc00::/7` |
| `linklocal` | `169.254.0.0/16`、`fe80::/10` |
| `10.42.0.0/16` | 显式 CIDR |

Bare IPs are rejected to keep intent explicit: use `127.0.0.1/32` or
`::1/128` instead. Unknown presets, invalid CIDRs, and `0.0.0.0/0` /
`::/0` all fail startup with a message naming the offending entry.

Rate limiting honors `X-Forwarded-For` only when the direct peer is inside
the trusted set. The list is then walked right-to-left, skipping every
trusted entry, and the nearest untrusted address becomes the client key.
Only list networks you actually control behind the reverse proxy; a wide
network means any host on it can forge the client IP seen by the limiter.

## Site verification and write-path authentication

`security.site_verification` controls the instance-wide policy:

Regardless of the policy, the `site_id` must be registered through the
API/CLI or declared in `[sites]` before it can be written to; unknown ids
return `404 code=site-not-registered`. Sites registered under a
caller-chosen id additionally need an origin verified before writes in
`optional` mode (random ids keep the relaxed behavior).

| Value | Unverified sites | Verified / configured sites |
|---|---|---|
| `disabled` | writes allowed, no trust checks (local development) | no checks |
| `optional` (default) | API-registered: allowed with WARN; no ownership proof: rejected | enforced |
| `required` | writes rejected | enforced |

In `optional` mode the WARN relaxation applies only to sites that were
registered through the API and still hold a claim token. A site whose
`[sites]` entry was removed (or a legacy/backfilled row with no claim token)
is rejected with `site-verification-required`, so removing configuration
tightens rather than opens the write path.

Per-site trust comes from two sources whose union is the effective rule set:

- **`[sites."<site_id>"]`** in this file: operator-declared trust. `auth_mode`
  is `"origin"` (default) or `"secret"`; `allowed_origins` accepts exact
  origins and `https://*.example.com` subdomain wildcards; `auth_mode =
  "secret"` requires a `secret` (HMAC key, at least 32 chars) that is best
  injected through `CUMMENTS__SITES__<site_id>__SECRET`.
- **Self-service registration** through `POST /api/v1/sites`: takes an
  optional, first-come `site_id` (a random id is generated when omitted) and
  returns a one-time claim token. The owner proves domain control via
  `/.well-known/cumments.json` or a DNS TXT record, then switches to secret
  auth via the secret endpoint. The CLI equivalent is
  `cumments sites register [--site-id ID]`. See [API](api/index.md).

Origin-mode requests are accepted only when the browser `Origin` matches the
effective allowlist; `Origin: null` and missing `Origin` are rejected. The
dev-only `disabled` policy is the exception: it accepts every origin,
including `Origin: null` from `file://` demo pages, and answers with
`Access-Control-Allow-Origin: *`. Secret-mode requests must carry
`X-Cumments-Timestamp` and `X-Cumments-Signature` (HMAC-SHA256 over
`timestamp\nMETHOD\npath\nsha256(body)`), with the timestamp within ±5
minutes.

## Rate limits

Every endpoint family's rate-limit budget is configurable under
`[rate_limit]`. Values are applied at startup (a restart is required) and
default to the historical hardcoded budgets:

```toml
[rate_limit]
registration = { requests = 10,  window = "1h" }
verification  = { requests = 20,  window = "1h" }
confirm       = { requests = 30,  window = "1h" }
operator         = { requests = 60,  window = "1m" }
write         = { requests = 120, window = "1h" }
sse           = { requests = 20,  window = "1h" }
media         = { requests = 120, window = "1h" }
visitor_profile = { requests = 120, window = "1h" }
public_read   = { requests = 1200, window = "1h" }
governance    = { requests = 60,  window = "1h" }
```

- `requests` is the maximum per window, per client key; it must be at least 1.
- `window` accepts human durations (`"500ms"`, `"30s"`, `"1h"`) and must be
  at least one second.
- Environment variables follow the usual mapping, e.g.
  `CUMMENTS__RATE_LIMIT__WRITE__REQUESTS`.
- The 429 `Retry-After` value derives from each endpoint's configured
  `window`.

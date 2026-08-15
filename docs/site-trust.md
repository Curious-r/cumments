# Site trust

Cumments' write API is public and `site_id` is a self-declared path
parameter. Site authentication binds a `site_id` to a real website and
enforces that binding on the write path (`POST`, `PATCH`, `DELETE`), so one
site cannot submit comments as another. It is **not** the comment author's
identity layer: every visitor still needs the PoW challenge and an Ed25519
signature for authorship, and every write request still carries an
`Idempotency-Key`.

Reading is always public. Comment lists (`QUERY`), SSE streams and the
challenge endpoint are served with `Access-Control-Allow-Origin: *`, because
comments are public data.

## Why authentication is needed

Without a binding, any client could pick any `site_id` and post comments to
it. The practical attackers are:

- **Other websites' pages** that embed a copied widget and post from their
  own origin;
- **Direct API callers** (scripts, bots, `curl`) that skip the browser
  entirely;
- **Site-id squatters** who claim an id before its legitimate owner.

Two independent mechanisms address these threats:

| Mechanism | Protects against | Does not protect against |
|---|---|---|
| **Origin mode** | Other websites' pages posting as your site — browsers cannot forge the `Origin` header | Scripts/`curl` forging `Origin`; XSS on the site; DNS/TLS compromise |
| **Secret mode** | Any client without the site secret, including scripts and `curl` | Site-secret leakage; compromise of the edge function; DNS/TLS compromise of the API link |

Origin mode is a **browser-context binding**, not cryptography: it proves
which website the browser is on. Secret mode is cryptographic: it proves
possession of the site's HMAC key.

## Two trust axes

Write enforcement is the product of an instance-wide policy and a per-site
auth mode.

### Instance-wide policy: `security.site_verification`

All three values share one prerequisite: the `site_id` must exist in the
registry (registered through the API/CLI or declared in `[sites]`). An
unknown id is rejected with `404 code=site-not-registered` before any trust
decision, so an unregistered site can never provision a Matrix Space.

| Value | Unverified sites | Verified or operator-configured sites |
|---|---|---|
| `disabled` | Writes allowed, no trust checks (local development) | No checks either |
| `optional` | API-registered: allowed with a WARN log; rows without ownership proof are rejected | Enforced |
| `required` | Writes rejected with verification guidance | Enforced |

`disabled` is the only mode that accepts the opaque `Origin: null` used by
`file://` demo pages. `optional` and `required` reject it, together with
missing or multiple `Origin` headers (see [Origin mode](#origin-mode)).

The `optional` relaxation only covers **API-registered** sites that simply
have not verified yet (they still hold a claim token). Two cases are rejected
instead, with `403 code=site-verification-required`:

- a caller-chosen `site_id` — a readable alias has to be backed by a real
  domain;
- a site row with no ownership proof at all — a removed `[sites]` entry, a
  legacy auto-created Space, or a row rebuilt by `backfill` after a database
  reset.

This keeps the migration escape hatch narrow and makes configuration removal
an immediate tightening: deleting a `[sites]` entry removes the only trust
anchor, so writes stop rather than falling back to "any origin accepted".

### Per-site auth mode: `sites.<id>.auth_mode`

| Value | Trust anchor | Requires |
|---|---|---|
| `origin` (default) | Browser `Origin` matched against verified/configured origins | A verified domain or an operator allowlist |
| `secret` | HMAC-SHA256 key held by the site backend | A site secret and a backend/edge function |

## The site registry

The effective trust rule set for a site is the **union** of two sources:

- **`[sites."<site_id>"]`** in the TOML configuration: operator-declared
  trust. This is declarative, version-controlled and human-reviewed, and the
  operator can skip the self-service verification flow entirely.
- **Database rows**: runtime state created by self-service registration and
  verification. These carry provenance (`config` or `verified`) so the Operator
  API can show where every allowed origin came from.

Configuration is never written back to by the API or the operator tooling.

The public registration endpoint accepts an **optional, first-come
`site_id`**; without one the server generates an unguessable random id. Every
registration returns a one-time **claim token**, and only its holder may
start verification for that site — which prevents a squatter from claiming a
victim's id and verifying it with a domain they control. Operator-configured
sites skip the claim step. Registration is mandatory in every policy: a site
must be operator-configured or registered through the API/CLI before it can
be written to, and legacy auto-creation of sites on first comment is gone.
Chosen ids carry a further obligation: they only become writable after an
origin is verified (see above), so a "pretty name" must be backed by a real
domain.

See [Site verification and strict mode](site-verification.md) for the full
registration, verification and key-issuance walkthrough.

## Origin mode

In origin mode, every write request must carry exactly one `Origin` header.
The middleware:

1. rejects missing, multiple, invalid or `null` origins with
   [`site-origin-denied`](problems/index.md#site-origin-denied);
2. normalizes the value (scheme + host, default port stripped, punycode,
   lowercase, no path);
3. matches it against the effective allowlist (operator config ∪ verified
   origins);
4. echoes the accepted origin in `Access-Control-Allow-Origin` and answers
   preflights for allowed origins.

The only supported wildcard is a subdomain form: `https://*.example.com`
matches any subdomain of `example.com` but not the apex. No implicit
localhost or private-network exceptions exist; local development uses
`site_verification = "disabled"`.

Origin-mode CORS is derived from the registry instead of being a global
config value. Preflights allow the write methods, and the allowed headers are
`content-type` and `idempotency-key`.

## Secret mode

Secret mode replaces the origin check with an HMAC signature, so even a
non-browser client cannot impersonate the site without the key. It is
intended for deployments that proxy comment submissions through their own
backend or edge function.

### Request signing

Every forwarded write request must carry:

- `X-Cumments-Timestamp`: Unix seconds;
- `X-Cumments-Signature`: hex HMAC-SHA256 over
  `timestamp\nMETHOD\npath\nsha256_hex(body)`,

with the site secret as the key. The timestamp must be within ±5 minutes of
the server clock. The secret is bound to the `site_id` in the request path;
an `Origin` header is not required.

The signature does **not** cover the `Host` header, so a secret must never
be shared between Cumments instances.

### Key lifecycle

- The key is generated and returned **exactly once** at issuance; it is never
  logged and never exposed through the API again.
- It is stored in the local database **in plain form**, because verifying an
  HMAC requires the key itself (unlike claim and verification tokens, which
  are stored only as SHA-256 hashes). Protect the database file like any
  other credential store.
- Keys can be rotated or revoked through the Operator API, and can also be
  declared directly in `[sites."<id>"]` with environment-variable injection.

Secret-mode sites do not accept browser preflights: a browser cannot call the
Cumments API directly with an HMAC. The site's own backend performs the
signing and forwards the request, so the frontend talks to its own origin.

## Effective behavior matrix

| Policy | `origin` mode | `secret` mode |
|---|---|---|
| `disabled` | Any *registered* site: origin accepted, `Access-Control-Allow-Origin: *` | Any *registered* site accepted (no HMAC check) |
| `optional` | API-registered unverified sites: accepted with WARN; rows without ownership proof are rejected; verified/config sites: origin must match | HMAC required whenever the site has a secret; sites without one follow the origin/unverified fallback |
| `required` | Origin must match a verified/config origin | HMAC required for any site configured with a secret; sites without a secret are rejected |

Read methods (`GET`, `QUERY`, SSE) are unaffected by this matrix and remain
public.

## Configuration

```toml
[security]
site_verification = "optional"   # disabled | optional | required
# Operator token for the Operator API (at least 32 chars); unset disables it.
# operator_token = "<random>"

[sites."test-blog"]
auth_mode = "origin"             # origin (default) | secret
allowed_origins = [
  "https://blog.example.com",
  "https://*.blog.example.com",
]
# auth_mode = "secret" requires a secret; prefer environment injection:
# CUMMENTS__SITES__test-blog__SECRET=...
```

Validation is fail-fast at startup:

- the removed `cors_origins` key is rejected as an unknown field instead of
  being silently accepted;
- origins are parsed and normalized; only `http(s)` schemes are allowed,
  without path/query/fragment, and the only wildcard is
  `https://*.example.com`;
- `auth_mode = "secret"` requires a non-empty, non-placeholder secret of at
  least 32 characters;
- `allowed_origins` is ignored (with a warning) when `auth_mode = "secret"`;
- `security.operator_token`, when set, must be at least 32 characters and not a
  known placeholder.

See [configuration.md](configuration.md) for the full reference.

## Operator operations

Database-tracked sites can be managed through the Operator API, authenticated
with `security.operator_token` sent as `Authorization: Bearer <token>`:

- list sites with origin provenance;
- revoke a verified origin;
- rotate or revoke an HMAC secret;
- export a TOML snippet to adopt a database-tracked site into `[sites]`.

Operator-configured origins and secrets cannot be changed through the API —
edit the configuration file instead. Operator routes are rate limited to 60
requests per minute per client.

## Security notes

- Origin mode is a browser-context control: a script or `curl` client can
  forge `Origin`, so it is only appropriate for pure frontend deployments.
- Secret mode moves the trust boundary to the key holder. Keep the key out of
  the site bundle and rotate it when an edge function or CI environment may
  have leaked.
- The HMAC secret is stored plaintext in the database by design; the
  database must be treated as a credential store.
- `Origin: null` is rejected outside `disabled` mode (a hardening class of
  CVE-2026-27978); `disabled` allows it only as the development exception.
- Write-path enforcement validates the `site_id` format before any database
  lookup, so malformed ids fail fast.

## Related documentation

- [Site verification and strict mode](site-verification.md) — walkthroughs
  for SSG sites, DNS/well-known proofs and edge-function examples.
- [Configuration](configuration.md) — the full configuration reference.
- [API](api/index.md) — endpoint reference, including registration, verification
  and secret issuance.
- [Problem types](problems/index.md) — the RFC 9457 error registry used by
  the write path.

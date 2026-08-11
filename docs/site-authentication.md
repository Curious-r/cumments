# Site Authentication & Origin Enforcement

Status: implemented (design record)
Date: 2026-08-12
Last updated: 2026-08-12 (post-review fixes)

Implementation status (2026-08-12):

- ✅ Phase 1 — data model (migration `m20260812_000016_site_authentication`,
  entities, `SiteAuthStore` port and implementation)
- ✅ Phase 2 — configuration (`site_verification`, `[sites."<id>"]`,
  `cors_origins` removed with an explicit error)
- ✅ Phase 3 — registration, verification and secret API under `/api/v1`,
  plus `cumments sites register`; verification endpoints are rate limited
- ✅ Phase 4 — enforcement middleware (origin + HMAC, derived CORS), routes
  migrated to `/api/v1`; `confirm` retries each proof location with backoff
- ✅ Phase 5 — admin surface (list, revoke origin, rotate/revoke secret,
  config-snippet export) behind `security.admin_token`
- ✅ Phase 6 — SSG verification walkthrough and strict-mode edge-function
  examples in [site-verification.md](site-verification.md)
- ✅ Phase 7 — router-level integration tests including a live well-known
  end-to-end flow, plus the post-review hardening: atomic idempotent
  verification completion, proof-download size caps, bounded rate limiter
  memory, and a 32-character admin token minimum. DNS lookup remains
  environment-dependent (parsing is unit tested)

## 1. Problem

The comment write API is public and `site_id` is a self-declared path
parameter. Today the only origin-related control is `server.cors_origins`,
which is a browser *read* policy: it decides whether a browser may read
responses, not whether the server accepts a request. Any non-browser client
can call the API with any `site_id` and a forged `Origin`.

Goal: let an operator (or a site owner) bind a `site_id` to a real website,
and enforce that binding on the write path.

## 2. Threat model

### Adversaries

- **Other websites' pages** embedding a copied widget (browser context).
- **Direct API callers** (curl, scripts, bots) posting without a browser.
- **Site-id squatters** claiming an id before its legitimate owner.

### What each mode protects

| Mode | Protects against | Does not protect against |
|---|---|---|
| `origin` (pure frontend) | Other websites' pages posting as your site; browsers cannot forge `Origin` | Scripts/curl forging `Origin`; DNS/TLS compromise; XSS on the site |
| `secret` (edge function) | Any client without the site secret, including scripts/curl | Site secret leakage; edge-function compromise; DNS/TLS compromise of the API link |

`origin` mode is a *browser-context binding*, not cryptographic site
authentication. `secret` mode is cryptographic: "whoever holds this site's
key". Both modes still rely on the existing visitor identity layer (PoW +
Ed25519) for comment authorship and anti-spam.

## 3. Design decisions

### 3.1 Two orthogonal axes

**Instance-wide policy** (`security.site_verification`):

| Value | Unverified sites | Verified / operator-trusted sites |
|---|---|---|
| `disabled` | writes allowed, no checks (dev/test) | no checks either |
| `optional` | writes allowed + WARN log (migration default) | enforced |
| `required` | writes rejected with guidance (production) | enforced |

**Per-site auth mode** (`sites.<id>.auth_mode`):

| Value | Trust anchor | Requires |
|---|---|---|
| `origin` (default) | browser `Origin` + domain verification | verified origins or operator allowlist |
| `secret` | site-held HMAC key | site backend / edge function |

### 3.2 Registry: config overlay + database state

The site registry is the **union** of two sources:

- `[sites."<id>"]` in the TOML file: operator-declared trust (declarative,
  version-controlled, human-reviewed).
- Database rows: runtime state from self-service registration/verification.

Configuration is never written back to. Every effective origin carries a
provenance marker (`config` | `verified`) for auditing and debugging.

### 3.3 Site claims (prevent id squatting)

Self-chosen `site_id`s are a first-come namespace; a squatter could claim a
victim's id and verify it with a domain they control, locking the victim out.
Mitigation:

- Sites registered through the API get a **server-generated random
  `site_id`** and a **claim token** (returned once, stored hashed). Only the
  claim-token holder may start verification for that site.
- Operator-configured sites (`[sites."<id>"]`) are operator-managed and skip
  the claim step.
- Legacy auto-created sites remain readable; writes keep working in
  `optional`/`disabled`. In `required` mode legacy auto-creation is
  **disabled entirely** (no escape hatch): sites must be operator-configured
  or registered through the API, and unclaimed sites cannot be written to.

### 3.4 Verification methods

- **well-known file (default)**: site owner publishes
  `/.well-known/cumments.json` containing `{ site_id, token }`. Cumments
  fetches it over HTTPS without following cross-host redirects.
- **DNS TXT**: owner publishes `_cumments.<host>` TXT records containing
  `site_id=...,token=...`. Cumments queries the system resolver (hickory).

The method is chosen **per verification attempt** (`start` request), not
configured. A `start` request takes a `methods` list, tried in the given
order. The same token is published in every chosen location; `confirm` tries
the methods in order with bounded retries/backoff and accepts the first one
that verifies. This gives the site owner resilience for free:
if the well-known file is temporarily unreachable (site down, network issue,
geo block) but the DNS TXT is visible, the retry succeeds, and vice versa.
All candidate proofs are equivalent in strength (each independently proves
domain control), so accepting either does not weaken security. Tokens are
single-use, bound to `(site_id, origin)`, and stored hashed. Verification can
be repeated to add origins; each origin is verified separately. One challenge
accepts at most 10 origins (duplicates are dropped), the well-known download
is capped at 1 MiB with a 5-second timeout, and `confirm` probes each
location twice with a 2-second delay.

### 3.5 Enforcement middleware (replaces `CorsLayer`)

CORS becomes a derived behavior, not a config value.

**Write methods** (`POST`, `PATCH`, `DELETE`):

- `disabled`: permissive, same as today.
- `origin` mode: require exactly one `Origin` header; reject `null` and
  missing; normalize (scheme+host, default port stripped, punycode, lowercase,
  no path); match against effective origins (config ∪ verified); otherwise
  `403` with a JSON body containing verification guidance. On success, echo
  the origin in `Access-Control-Allow-Origin` and answer preflights for
  allowed origins.
- `secret` mode: verify the HMAC credential only —
  `X-Cumments-Timestamp` + `X-Cumments-Signature` (HMAC-SHA256 over
  `timestamp\nMETHOD\npath\nsha256_hex(body)`), with a ±5 min timestamp
  window. The signature does not cover the `Host` header, so a secret must
  never be shared between Cumments instances.
  The secret is bound to the `site_id` in the path. `Origin` is not required.
  No browser CORS path (frontend calls its own backend).

**Decision: HMAC only, no static bearer token.** A bearer token would add a
second credential path (and its own leakage surface) without adding security;
HMAC already provides possession proof, request integrity, and replay
bounding, and the edge-function examples are barely more complex. One
credential path also means one implementation, one test matrix, one doc story.
Issuance lives in Phase 3, request verification in Phase 4, rotation in
Phase 5.

The HMAC key is stored in the local database **in plain form** — verifying an
HMAC signature requires the key itself, not its hash. The raw key is returned
to the owner exactly once at issuance, never logged, and never exposed through
the API again. Claim tokens and verification tokens, by contrast, are stored
only as SHA-256 hashes. The database file must be protected like any other
credential store.

**Read methods** (`GET`, `QUERY`, `SSE`): public. `Access-Control-Allow-Origin:
*`; preflights allow `GET`, the custom `QUERY` method, and `content-type`.

There are no implicit localhost exceptions; local development uses
`site_verification = "disabled"`.

### 3.6 Configuration surface

```toml
[server]
host = "0.0.0.0"
port = 7931
# cors_origins removed: CORS is derived from the site registry

[security]
pow_secret = "change-me"
pow_difficulty = 4
site_verification = "optional"   # disabled | optional | required
# Operator token for the admin API (at least 32 chars); unset disables it.
# admin_token = "<random>"

[sites."test-blog"]
auth_mode = "origin"             # origin (default) | secret
allowed_origins = [
  "https://blog.example.com",
  "https://*.blog.example.com",
]
# auth_mode = "secret" requires a secret; prefer environment injection:
# CUMMENTS__SITES__test-blog__SECRET=...
```

Validation rules (fail fast at startup):

- `deny_unknown_fields` stays; `cors_origins` produces an explicit error
  explaining the replacement instead of being silently accepted.
- Origins are parsed and normalized; only `http(s)` schemes, no path/query/
  fragment; the only wildcard form is `https://*.example.com`.
- `auth_mode = "secret"` requires a non-empty, non-placeholder secret of
  minimum length (32 chars);
  the raw key is held in memory/DB for HMAC verification, never
  logged, and never returned after issuance.
- `allowed_origins` is ignored (with a validation warning) when
  `auth_mode = "secret"`.
- `security.admin_token` is optional; when set it must be at least 32
  characters and not a known placeholder. The admin API stays disabled when
  it is absent.

### 3.7 Data model (database)

- `sites`: add `auth_mode`, `verification_status`, `claim_token_hash`,
  `secret` (the HMAC key itself, needed for verification), `verified_at`,
  `updated_at`.
- `site_verified_origins`: `(site_id, origin, created_at)` — rows are
  verified (self-service) origins; config origins are merged at load time and
  carry no rows. Provenance (`config` | `verified`) is derived, not stored.
- `verification_tokens`: `(id, site_id, origin, token_hash, methods,
  expires_at, consumed_at, created_at)` — ephemeral, hashed, single-use.

### 3.8 Admin surface

Database-tracked sites need management operations: list, show provenance,
revoke an origin, rotate/issue a secret, adopt into config. This requires an
operator-authenticated admin API or CLI.

**Decision: admin transport uses an operator token** (`security.admin_token`,
commonly injected as `CUMMENTS__SECURITY__ADMIN_TOKEN`, sent in
`Authorization: Bearer`). Rationale:
it is simple, standard, works in `logging` mode and when the homeserver is
down, and is easy to rotate. Matrix owner identity (`matrix.moderation.
owner_id`) remains a possible future *alternative login* so admin actions can
be attributed to a Matrix account, but it is not the v1 mechanism: it depends
on homeserver availability and requires a signed-request or access-token
scheme that adds complexity without changing the authorization model.

If Matrix identity is ever added, the realistic bridges are:

- **Access-token validation**: the admin client logs in to the homeserver as
  the owner and presents the Matrix access token; Cumments calls
  `/_matrix/client/v3/account/whoami` (as the appservice) to verify the token
  and that the user id equals `owner_id`.
- **Command messages through the appservice**: the owner sends bot commands
  (`!cumments sites list`) into a management room; the homeserver vouches for
  the sender via push events, so Cumments only needs to compare the sender
  mxid with `owner_id`. No HTTP credential involved.

Both are heavier than a static operator token; neither is in v1.

Implemented operations: list sites with origin provenance, revoke a verified
origin, rotate/revoke the HMAC secret, and export a TOML snippet for adopting
a database-tracked site into `[sites]`. Config-declared origins and secrets
cannot be changed through the API (edit the config file instead), rotation of
a missing site returns 404, and admin routes are rate limited to 60
requests/minute per client.

### 3.9 API versioning

**Decision: all public API moves to `/api/v1/...`** — comments, challenge,
verification, secret issuance. `/health` stays unversioned as an
infrastructure endpoint. No compatibility alias is kept (project is pre-1.0).

Rationale from industry research:

- Path versioning (`/api/v1`) is the most common convention among major
  public APIs. Google's API design guide mandates the major version as the
  first URI path segment (`/v1/...`, `v` prefix, ordinal number); Stripe uses
  `/v1` in the URL and layers dated minor versions on top; Microsoft lists URL
  path as the primary option.
- Header/media-type versioning (GitHub's `X-GitHub-Api-Version`, Zalando's
  media-type rule) is more REST-pure but hides the version from clients,
  requires a default-version policy, and complicates tooling and caching.
  It pays off for APIs with many long-lived clients; we do not have that
  constraint.
- Query-parameter versioning is broadly discouraged (caching and visibility
  problems).
- Google's own guidance warns against adding `/v1` preemptively if no breaking
  change is expected; we are introducing breaking changes now (new auth model,
  verification endpoints, HMAC), so pre-1.0 is the cheapest moment to version.
- Versioning strategy going forward: **evolve within v1** (additive
  non-breaking changes only); if a breaking change becomes necessary, ship
  `/api/v2` with a documented deprecation window rather than changing `/v1`.

## 4. Implementation plan

All phases below are implemented as of 2026-08-12; the list is kept as a
design record.

### Phase 0 — Design docs

- Land this document; update `docs/configuration.md` with the new schema.
- Acceptance: design reviewed; config reference matches the code plan.

### Phase 1 — Data model

- SeaORM migrations for `sites` columns, `site_verified_origins`,
  `verification_tokens`.
- Repository methods: effective origin merge (config ∪ DB), origin
  add/revoke, claim token issue/consume, secret hash store/rotate.
- Acceptance: unit tests for merge and provenance.

### Phase 2 — Configuration

- Add `site_verification` enum and `[sites."<id>"]` table; remove
  `cors_origins`; add normalization and validation (including the explicit
  `cors_origins` error).
- Update all embedded example configs and `docs/configuration.md`.
- Acceptance: `deny_unknown_fields` tests; invalid origins fail startup;
  env-var overrides work for per-site secrets.

### Phase 3 — Registration & verification API

- All new endpoints live under `/api/v1/...`; existing public routes
  migrated to `/api/v1` with no compatibility alias.
- `POST /api/v1/sites` (registration): returns random `site_id` + claim token.
- Claim token is also issued through a CLI (`cumments sites register`) — the
  API and CLI share one code path.
- `POST /api/v1/sites/{id}/verifications` (start): methods
  (`well-known` | `dns`) and origin(s); returns token + exact instructions.
- `POST /api/v1/sites/{id}/verifications/confirm`: fetches file / queries
  DNS, validates, records verified origins.
- `POST /api/v1/sites/{id}/secret` (for verified sites): generates an HMAC
  key, returns it exactly once, stores it for signature verification.
- Rate-limit verification endpoints.
- Acceptance: happy path for both methods; token reuse and expiry rejected;
  wrong-domain proof rejected; squatter cannot verify without claim token.

### Phase 4 — Enforcement middleware

- Replace `CorsLayer` with per-route/per-method middleware implementing 3.5.
- Implement the single HMAC credential path for `auth_mode = "secret"`
  (`X-Cumments-Timestamp` + `X-Cumments-Signature`, ±5 min window).
- Preserve current read behavior; write behavior follows policy × auth mode.
- Acceptance: enforcement matrix tests (below) all pass; demo works in
  `disabled`; existing compose example remains runnable.

### Phase 5 — Admin surface

- Operator-token-authenticated list/revoke/rotate/adopt operations.
- Secret rotation and revocation for database-tracked sites.
- Acceptance: full lifecycle of a database-tracked site without SQL.

### Phase 6 — Docs & SSG integration

- Threat-model page (this document's §2, user-facing).
- Verification walkthroughs: well-known file for Hugo/Next/11ty, DNS TXT.
- Strict-mode examples: Cloudflare Pages Functions, Netlify Functions,
  Vercel Functions (small proxy with env secret).
- Migration guide: `optional` → `required`; demo update notes.
- Acceptance: a fresh SSG user can complete verification from the docs.

### Phase 7 — Hardening & test matrix

Security regression tests:

- `Origin: null` is rejected (Next.js CVE-2026-27978 class).
- Multiple `Origin` headers rejected; missing `Origin` on writes rejected
  (origin mode).
- Forged `Origin` via curl rejected in `secret` mode; accepted in `origin`
  mode (documented ceiling).
- HMAC replay outside the timestamp window rejected; wrong-site secret
  rejected.
- Normalization: default ports, uppercase hosts, trailing slashes, punycode,
  wildcard subdomain matching.
- Policy matrix: `disabled`/`optional`/`required` × `origin`/`secret` ×
  browser/non-browser × verified/unverified/operator-trusted.

## 5. Decision log

Resolved decisions:

- Wildcard subdomains (`https://*.example.com`) ship in v1.
- Claim-token UX: API **and** CLI, sharing one implementation.
- Verification method is a per-attempt choice, never a config value; no
  persistent configuration, but `start` accepts a `methods` list and `confirm`
  retries them in order, so a second proof acts as an automatic fallback
  (see §3.4).
- Admin transport uses an operator token; Matrix owner identity is a
  possible future alternative login (see §3.8).
- Secret-mode credentials: HMAC only, no bearer token; issuance in Phase 3,
  request verification in Phase 4, rotation in Phase 5.
- `required` mode disables legacy auto-creation entirely, no escape hatch.
- Public API versioning: `/api/v1`, `/health` unversioned, no compat alias
  (see §3.9).
- Admin token minimum length is 32 characters; admin routes are rate limited
  to 60/min per client (see §3.8).
- Verification challenges accept at most 10 origins (deduplicated); the
  well-known proof download is capped at 1 MiB; `confirm` makes 2 attempts
  per location with a 2-second backoff and a 5-second fetch timeout.
- Verification completion (`consume token + record origin + mark verified`)
  is a single idempotent transaction, so concurrent confirmations cannot
  fail or double-record.
- Rate limiters are in-memory sliding windows with a fixed key cap
  (registration 10/h, verification issuance 20/h) — anti-spam, not a
  security boundary.
- The write middleware validates the `site_id` format before any database
  lookup, and plain-HTTP warnings cover wildcard patterns too.

Out of scope for v1: Matrix-identity admin login, non-breaking evolution
within `/api/v1` is the norm and `/api/v2` is the escape hatch for breaking
changes.

# API

All public endpoints live under `/api/v1`; `/health` is unversioned.

The endpoint reference is split by resource area:

- [Comments](comments.md) — list, post, edit, delete, reactions, poll votes,
  locations and room info.
- [Sites](sites.md) — self-service registration, verification and HMAC
  secret issuance.
- [Governance](governance.md) — site admins, managers, room moderators, room
  upgrades and the projected rosters.
- [Visitors](visitors.md) — the visitor's self-service profile read.
- [Operator](operator.md) — operator-only endpoints.
- [Media](media.md) — the public media proxy, visitor uploads, visitor avatars
  and site sticker packs.

The sections below describe the primitives shared by every endpoint.

## Challenge

`GET /api/v1/challenge`

```json
{
  "prefix": "timestamp_hex.random_hex.signature",
  "difficulty": 4
}
```

Challenges expire after 5 minutes.
Responses are marked `Cache-Control: no-store` (plus `Pragma: no-cache`) so
intermediaries never serve a stale challenge.

## Health

`GET /health`

```json
{ "status": "ok" }
```

## Authors

All write operations require `author_public_key` (base64url Ed25519, 32 bytes)
and `author_signature` over a canonical message. The PoW `challenge_prefix`
is the part of `challenge_response` before `|`.

Authors come in two forms:

- `"type": "visitor"` — posted through the Cumments API by a virtual user;
  `author.public_key` is set and `PATCH`/`DELETE` work via the API.
- `"type": "matrix"` — posted directly in Matrix by a regular account;
  `author.mxid` is set. These comments are managed from a Matrix client, and
  the Cumments API returns `403 code=not-manageable` for `PATCH`/`DELETE`.

## Idempotent writes

Comment post/edit/delete/location, visitor media upload, and visitor avatar
upload are durable submissions. Comment/location submissions accept work and
return `202 { "submission_id": ... }` before the comment actually lands in
Matrix; media and avatar uploads return their result synchronously.
If the client loses the response (network failure, timeout, browser crash) it
can retry the exact same request with the same `Idempotency-Key` header; the
server detects the duplicate and returns the original `submission_id` again with
`Idempotent-Replayed: true`, without queueing a second submission.

`202` is a local queue acknowledgement: the submission is durably persisted
in Cumments' SQLite database and will be converged to Matrix while that
record survives. It is not proof that the comment exists in Matrix yet.
Matrix becomes authoritative only when the event is written; if the local
database is lost before then, the submission may be lost and `backfill`
cannot recover it because no Matrix event was created. See
[Architecture](../architecture.md#submission-durability).

Rules:

- The key is mandatory (missing or invalid values return
  `400 code=idempotency-key-required` / `400 code=invalid-idempotency-key`).
- Keys are scoped to `author_public_key + Idempotency-Key`; the same key from
  a different author is independent.
- The request fingerprint is `METHOD\npath\nsha256(body)` (media uploads also
  include `mime` and `filename`). Reusing a key with a different request
  returns `409 code=idempotency-key-reused`; the conflicting request is not
  recorded and not queued.
- Invalid requests (bad PoW, bad signature, not found, unauthorized, invalid
  JSON) do not consume the key.
- Records are kept for 24 hours, aligned with Stripe's idempotency retention;
  after that the key can be reused.

Reactions (add and remove), poll votes, and visitor avatar DELETE are
natural-idempotent synchronous operations and do not use this header.
Reaction/vote (and unreact) retries derive a deterministic Matrix transaction
ID from the exact signed request and PoW challenge. The PoW challenge itself
is single-use, so an HTTP retry after an accepted action returns invalid-PoW
rather than duplicating the effect; avatar DELETE simply sets the profile
field to absent again. Reaction removal resolves the annotation by
`(comment, virtual user, key)` without exposing Matrix event IDs.

Clients should generate a fresh key per logical write (e.g. `crypto.randomUUID()`)
and reuse that exact key when retrying the same request. Use the same endpoint
form (the path-based or body-based PATCH variant) for all retries of a key.

## Real-time updates (SSE)

`GET /api/v1/sites/{site_id}/pages/{page_slug}/sse`

Server-sent events use the shape `{ "type": "...", "payload": { ... } }`:

```text
type: message_created
type: message_updated
type: message_deleted
type: message_annotations_changed
type: ephemeral
```

The `message_created` and `message_updated` payloads contain the full
`Message` object (the same shape as the [list response](comments.md#list-comments));
`message_deleted` contains the deleted `event_id` and, when the deletion went
through the Cumments API, the `submission_id`. `message_annotations_changed`
signals that reaction or poll counts changed; `ephemeral` carries live room
state such as typing indicators:

```json
{ "type": "typing", "room_id": "!room:server", "user_id": "@alice:server", "typing": true, "display_name": "Alice" }
```

Typing events also arrive as an initial snapshot on connect. Read receipts
and presence are forwarded when the homeserver exposes them; clients
may choose to only render typing. See
[Ephemeral events](../data-model.md#ephemeral-events) for the channel's
limits.

## Error responses

All error responses use the RFC 9457 problem details format with
`Content-Type: application/problem+json`:

```json
{
  "type": "https://curious-r.github.io/cumments/problems/#idempotency-key-reused",
  "title": "Idempotency-Key reused",
  "status": 409,
  "detail": "This Idempotency-Key was already used with a different request.",
  "code": "idempotency-key-reused"
}
```

`code` is a stable machine-readable slug; `type` is its canonical URI
and resolves to the problem documentation on the docs site. The complete
registry is documented in [Problem types](../problems/index.md).

## Rate limiting

`POST /api/v1/sites` and `POST /api/v1/sites/{site_id}/verifications` are
rate limited per client IP (10/hour and 20/hour by default). Limit exceeded
returns `429 code=rate-limited`. Verification `confirm` is limited to
30/hour, comment and visitor-avatar writes (`POST`/`PUT`/`PATCH`/`DELETE`) to
120/hour, visitor profile reads to 120/hour, claim-token authentication
attempts to 60/minute, and SSE connection attempts to a bounded token-bucket
budget of 120/hour with a burst of 8 by default. A global semaphore caps the
instance at 500 concurrently open streams. Local public reads (comment lists,
room metadata, roles, moderators, sticker packs) share a generous
`public_read` budget of 1200/hour per client key, which covers normal page
loads without the anonymous-low-quota problems Giscus-style embeds hit on
GitHub's 60/hour limit. Site governance writes are limited to 60/hour after
authentication. Every budget is configurable under `[rate_limit]` and applied
at startup; see [Configuration](../configuration.md#rate-limits).

Rate limiting is layered by cost: endpoints that trigger external work
(media proxy, visitor profile lookups) or long-lived connections (SSE) get
their own tighter budgets, while local SQLite reads get the generous
`public_read` bucket. In front of a reverse proxy or CDN, that layer remains
the primary defence for high-frequency public traffic.
Every `429` response carries a `Retry-After` header set to the endpoint's
fixed limit window (3600 seconds for hourly limits, 60 seconds for the operator
API). It is a conservative constant, not the exact remaining time for the
requesting client.
SSE uses a token bucket rather than separate reconnect bookkeeping: the burst
capacity absorbs EventSource auto-reconnects and normal page refreshes, while
the sustained rate prevents connection floods. A global semaphore separately
bounds concurrently open streams.

Client keys are the peer IP by default. `X-Forwarded-For` is honored only
when the peer is inside a `server.trusted_proxies` preset or CIDR; the list
is then walked right-to-left, skipping trusted proxies, and the nearest
untrusted IP address is used as the client key. A malformed untrusted value
falls back to the direct peer address instead of becoming a limiter key.

Claim-token endpoints use a separate pre-auth admission bucket. Missing,
invalid, and valid-looking attempts all consume it before the token hash is
loaded; successful owner operations then consume their normal endpoint/governance
budget.

Verification origins must be public by default: loopback/private/link-local
IP-literal origins are rejected unless
`security.allow_private_verification_origins = true`. Each verification
token allows at most 5 confirm attempts before a new challenge is required.

## Validation

`site_id` and `page_slug` accept lowercase `[a-z0-9-]`, 1–64 characters.
Invalid values return `400 code=validation-error`.

## Design trade-offs

These are deliberate choices, kept here so callers understand why the API
looks the way it does.

**QUERY instead of GET with a body.** List endpoints take pagination in a
JSON request body, so they use the `QUERY` method (RFC 10008) rather than
`GET`. GET bodies are dropped by some intermediaries and discouraged by the
HTTP spec; QUERY carries the payload while staying safe and cacheable. The
API advertises `Accept-Query: application/json` and returns
`405 code=method-not-allowed` for GET.

**DELETE with author proof in the body.** RFC 9110 leaves DELETE
request-body semantics undefined and some intermediaries strip it. Cumments
nevertheless carries the author proof (`author_public_key`,
`author_signature`, `challenge_response`) in the body for visitor-authored
mutations (`DELETE /comments/{comment_id}` and
`DELETE /comments/{comment_id}/reactions/{key}`) because the proof must be
end-to-end verifiable and is not cacheable. Operator and governance
`DELETE` operations that require no author proof remain body-free and use
path or query parameters.

**Missing parents are 404, empty children are 200.** A site that is not
registered returns `404` for its nested resources (comment list, roles,
sticker packs, visitor profile, room, moderators) because the parent resource
does not exist. Once the site exists, "no data yet" is still a successful
empty response: an empty comment page, an empty role/sticker list, or an
empty visitor profile (`null` fields) all come back as `200`.

**Registration before writes.** A `site_id` must be registered through the
site API/CLI or declared in `[sites]` before it can receive comments, in
every verification policy. This keeps an unknown id from provisioning a
Matrix Space on its first comment, which would turn an open registration
endpoint into unbounded homeserver resource use. See
[Site trust](../site-trust.md). Caller-chosen ids add one more requirement:
in `optional` mode they must verify an origin before writes, so a readable
alias has to be backed by a real domain; the same applies to any row without
an ownership proof (a removed `[sites]` entry, a legacy Space, or a backfill
rebuild), so the optional-mode relaxation is reserved for API-registered
sites.

**403 for authentication failures.** Missing or invalid claim tokens, origin
mismatches, and unauthorized writes return `403` with a stable problem
`code`, not `401`. Site authentication is origin/HMAC based rather than HTTP
authentication, so there is no `WWW-Authenticate` challenge to advertise and
clients must not prompt for credentials. Operator token failures use `403` the
same way, so the API never emits `401`.

**Constant `Retry-After` windows.** Rate limiters are in-memory, per-client
sliding windows (keyed by peer IP, with trusted-proxy-aware `X-Forwarded-For`
parsing). A `429` advertises the endpoint's fixed window as `Retry-After`
rather than the exact remaining time for that client: it is conservative,
simple, and does not leak per-key limiter state. Multi-instance deployments
would need a shared limiter store — a documented platform limitation.

**Asynchronous write submissions.** `POST`/`PATCH`/`DELETE` enqueue a submission
and return `202 { "submission_id" }` before the comment lands in Matrix. This
keeps request latency bounded by the queue write, decouples clients from
homeserver timing, and pairs with `Idempotency-Key` (scoped to the author's
public key, 24-hour retention) to make retries safe.

**Mutations return the affected resource.** Write endpoints return the
affected resource as JSON (the updated site, the pending role claim, the
revoked role). `DELETE /api/v1/operator/quarantined-rooms/{room_id}` is the
single exception and returns `204`: the quarantine row is gone, so there is
no surviving resource to serialize.

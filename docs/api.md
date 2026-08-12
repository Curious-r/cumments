# API

All public endpoints live under `/api/v1`; `/health` is unversioned.

## Challenge

`GET /api/v1/challenge`

```json
{
  "prefix": "timestamp_hex.random_hex.signature",
  "difficulty": 4
}
```

Challenges expire after 5 minutes.

## Health

`GET /health`

```json
{ "status": "ok" }
```

## Comments

All write operations require `author_public_key` (base64url Ed25519, 32 bytes)
and `author_signature` over a canonical message. The PoW `challenge_prefix`
is the part of `challenge_response` before `|`.

Authors come in two forms:

- `"type": "guest"` — posted through the Cumments API by a virtual user;
  `author.public_key` is set and `PATCH`/`DELETE` work via the API.
- `"type": "matrix"` — posted directly in Matrix by a regular account;
  `author.mxid` is set. These comments are managed from a Matrix client, and
  the Cumments API returns `403 NOT_MANAGEABLE` for `PATCH`/`DELETE`.

### List comments

`QUERY /api/v1/sites/{site_id}/posts/{post_slug}/comments` (RFC 10008)

Body:

```json
{ "page": 1, "per_page": 20 }
```

Response:

```json
{
  "data": [
    {
      "event_id": "$event:server",
      "site_id": "my-blog",
      "post_slug": "hello-world",
      "author": {
        "type": "guest",
        "display_name": "Alice",
        "public_key": "...",
        "mxid": null
      },
      "content": "...",
      "timestamp": "2026-08-08T00:00:00Z"
    }
  ],
  "meta": {
    "total": 1,
    "page": 1,
    "per_page": 20,
    "total_pages": 1
  }
}
```

### Post a comment

`POST /api/v1/sites/{site_id}/posts/{post_slug}/comments`

Write requests are gated by site authentication: either the browser `Origin`
must match the site's verified/configured origins, or (secret mode) the
request must carry `X-Cumments-Timestamp` and `X-Cumments-Signature`
(HMAC-SHA256 over `timestamp\nMETHOD\npath\nsha256(body)`, ±5 minutes).
See [configuration](configuration.md) for the policy.

Body:

```json
{
  "content": "...",
  "display_name": "Alice",
  "author_public_key": "...",
  "author_signature": "...",
  "challenge_response": "challenge|nonce"
}
```

Signature message:

```text
POST\n{site_id}\n{post_slug}\n{content}\n{display_name}\n{reply_to}\n{challenge_prefix}
```

`reply_to` is the exact Matrix event ID of the parent comment as returned by
the API, or an empty line when the comment is not a reply. Event IDs are
opaque strings by spec; legacy v1/v2 IDs look like `$localpart:server` while
room v3+ IDs are bare hashes (v3 may even contain `/`). When an event ID is
used in a request path (edit/delete), clients must percent-encode it.

### Edit a comment

`PATCH /api/v1/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

Signature message:

```text
PATCH\n{site_id}\n{post_slug}\n{comment_id}\n{content}\n{challenge_prefix}
```

### Delete a comment

`DELETE /api/v1/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

Signature message:

```text
DELETE\n{site_id}\n{post_slug}\n{comment_id}\n{challenge_prefix}
```

## Real-time updates (SSE)

`GET /api/v1/sites/{site_id}/posts/{post_slug}/sse`

Server-sent events use the shape `{ "type": "...", "payload": { ... } }`:

```text
type: comment_created
type: comment_updated
type: comment_deleted
```

The `comment_created` and `comment_updated` payloads contain the full `Comment`
object; `comment_deleted` contains the deleted `event_id`.

## Site registration and verification

### Register a site

`POST /api/v1/sites`

Returns a random, unguessable `site_id` and a one-time `claim_token`:

```json
{ "site_id": "3f9c...", "claim_token": "..." }
```

The claim token proves ownership of the site and must be sent in the
`X-Cumments-Claim-Token` header for verification and secret issuance. It is
shown once and only its hash is stored.

### Start verification

`POST /api/v1/sites/{site_id}/verifications`

Headers: `X-Cumments-Claim-Token: <claim_token>`

Body:

```json
{
  "origins": ["https://blog.example.com"],
  "methods": ["well-known", "dns"]
}
```

`methods` are tried in order by `confirm`; publishing the same token in every
chosen location gives an automatic fallback. The response contains the token,
the expiry, and concrete publishing instructions.

### Confirm verification

`POST /api/v1/sites/{site_id}/verifications/confirm`

Body:

```json
{ "origin": "https://blog.example.com", "token": "..." }
```

Cumments fetches `{origin}/.well-known/cumments.json` and/or queries the
`_cumments.<host>` TXT record. On the first matching proof it records the
origin and returns the updated `verified_origins` list.

Well-known document shapes (both accepted):

```json
{ "site_id": "...", "token": "..." }
```

```json
{ "sites": [ { "site_id": "...", "token": "..." } ] }
```

DNS TXT value format:

```text
site_id=<site_id>,token=<token>
```

### Issue an HMAC secret (strict mode)

`POST /api/v1/sites/{site_id}/secret`

Headers: `X-Cumments-Claim-Token: <claim_token>`

Body: `{ "rotate": false }` (omit to issue; `true` replaces an existing
secret).

The site must be verified first. The secret is returned exactly once:

```json
{ "site_id": "...", "secret": "..." }
```

It is used as the HMAC key in edge-function deployments (see
[site-authentication.md](site-authentication.md)); the same value must be set
on the site backend and used to sign every write request.

## Admin API

Enabled by setting `security.admin_token`. All admin routes require
`Authorization: Bearer <token>`.

Admin routes are rate limited (60 requests/minute per client key).

### List sites

`GET /api/v1/admin/sites`

Returns every database-tracked site merged with the operator-declared
`[sites]` overlay. Each origin carries a `source` of `"verified"` or
`"config"`.

### Revoke a verified origin

`POST /api/v1/admin/sites/{site_id}/origins/revoke`

Body: `{ "origin": "https://blog.example.com" }`. Origins declared in the
config file cannot be revoked here — edit the config instead.

### Rotate / revoke the HMAC secret

`POST /api/v1/admin/sites/{site_id}/secret/rotate` — returns the new secret
exactly once.

`DELETE /api/v1/admin/sites/{site_id}/secret` — removes the secret and falls
back to origin auth.

Both refuse to touch sites whose secret is declared in the config file.

### Export an adoption snippet

`GET /api/v1/admin/sites/{site_id}/config-snippet`

Returns a TOML block to paste into `[sites]` when the operator wants to move a
database-tracked site into declarative config.

### Rotate the claim token

`POST /api/v1/admin/sites/{site_id}/claim-token/rotate`

Returns a new `claim_token` exactly once and invalidates the previous token.
Use this when a claim token may have leaked.

### List blocked rooms

`GET /api/v1/admin/rooms/blocked`

Returns rooms whose adoption failed governance/room-version checks, with the
room id, site/post, reason, and last update. A successful re-registration
clears the blocked state.

## Rate limiting

`POST /api/v1/sites` and `POST /api/v1/sites/{site_id}/verifications` are
rate limited per client IP (10/hour and 20/hour). Limit exceeded returns
`429 RATE_LIMITED`. Verification `confirm` is limited to 30/hour, comment
writes (`POST`/`PATCH`/`DELETE`) to 120/hour, and new SSE connections to
20/hour with a global cap of 500 concurrent streams.

Client keys are the peer IP by default. `X-Forwarded-For` is honored only
when the peer is listed in `server.trusted_proxies`; the first value is then
used as the client key.

Verification origins must be public by default: loopback/private/link-local
IP-literal origins are rejected unless
`security.allow_private_verification_origins = true`. Each verification
token allows at most 5 confirm attempts before a new challenge is required.

## Validation

`site_id` and `post_slug` accept lowercase `[a-z0-9-]`, 1–64 characters.
Invalid values return `400 VALIDATION_ERROR`.

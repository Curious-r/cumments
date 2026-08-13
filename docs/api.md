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
Responses are marked `Cache-Control: no-store` (plus `Pragma: no-cache`) so
intermediaries never serve a stale challenge.

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
  the Cumments API returns `403 code=not-manageable` for `PATCH`/`DELETE`.

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
        "avatar_url": null,
        "public_key": "...",
        "mxid": null
      },
      "content": {
        "type": "text",
        "body": "hello **world**",
        "formatted_body": "<p>hello <strong>world</strong></p>",
        "style": "normal"
      },
      "timestamp": "2026-08-08T00:00:00Z",
      "room_id": "!room:server",
      "sender_mxid": "@_cumments_my-blog_3282f2a21b4a1e6b:server",
      "edited_at": null,
      "reply_to": null,
      "thread_root": null,
      "intent_id": 42,
      "status": "active",
      "redacted_at": null,
      "redacted_by": null,
      "reactions": [
        { "key": "👍", "count": 2 }
      ],
      "raw_content": {}
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

`content` is a typed object; the other variants look like:

- `media`: `{ "type": "media", "kind": "image|video|audio|file|sticker", "url": "mxc://… or /api/v1/media/…", "filename": …, "mimetype": …, "size": …, "width": …, "height": …, "thumbnail_url": …, "alt_text": …, "voice": false }`
- `location`: `{ "type": "location", "geo_uri": "geo:30.2,120.1", "description": …, "thumbnail_url": … }`
- `poll`: `{ "type": "poll", "question": …, "options": [{ "id": …, "text": … }], "responses": [{ "option_index": 0, "count": 3 }] }`
- `encrypted`: `{ "type": "encrypted", "algorithm": "m.megolm.v1.aes-sha2", "sender_key": … }`
- `unknown`: `{ "type": "unknown", "fallback": …, "raw": { … } }`

Media URLs are rewritten to signed proxy URLs when the media proxy is
enabled; see [Media proxy](#media-proxy) below.

### Post a comment

`POST /api/v1/sites/{site_id}/posts/{post_slug}/comments`

Write requests are gated by site authentication: either the browser `Origin`
must match the site's verified/configured origins, or (secret mode) the
request must carry `X-Cumments-Timestamp` and `X-Cumments-Signature`
(HMAC-SHA256 over `timestamp\nMETHOD\npath\nsha256(body)`, ±5 minutes).
See [configuration](configuration.md) for the policy.

Every write request must also carry an `Idempotency-Key` header (8-255
printable ASCII characters). See [Idempotent writes](#idempotent-writes)
below.

Body:

```json
{
  "content": "...",
  "media": null,
  "display_name": "Alice",
  "author_public_key": "...",
  "author_signature": "...",
  "reply_to": null,
  "challenge_response": "challenge|nonce"
}
```

When `media` is present (an object returned by
[Guest media upload](#guest-media-upload), or a preset sticker reference
with `"kind": "sticker"`), the signature covers `media.url` instead of
`content`; `content` then only serves as the fallback filename/text.

Successful writes are asynchronous and return `202` with the queue row ID:

```json
{ "intent_id": 42 }
```

The projected comment (list/SSE `message_created`) carries the same
`intent_id` when it was submitted through the Cumments API, so clients can
correlate the accepted request with the final comment. Matrix-native comments
omit `intent_id`.

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

The same operation is available without embedding `comment_id` in the URL:

`PATCH /api/v1/sites/{site_id}/posts/{post_slug}/comments`

```json
{
  "comment_id": "$event:server",
  "content": "edited",
  "author_public_key": "...",
  "author_signature": "...",
  "challenge_response": "challenge|nonce"
}
```

The path-based form remains supported for backwards compatibility.

Both edit forms require the `Idempotency-Key` header.

### Delete a comment

`DELETE /api/v1/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

Signature message:

```text
DELETE\n{site_id}\n{post_slug}\n{comment_id}\n{challenge_prefix}
```

The body-based form is:

`DELETE /api/v1/sites/{site_id}/posts/{post_slug}/comments`

```json
{
  "comment_id": "$event:server",
  "author_public_key": "...",
  "author_signature": "...",
  "challenge_response": "challenge|nonce"
}
```

The path-based form remains supported for backwards compatibility.

Both delete forms require the `Idempotency-Key` header.

### Idempotent writes

`POST`, `PATCH` and `DELETE` are asynchronous: they accept an intent and
return `202 { "intent_id": ... }` before the comment actually lands in Matrix.
If the client loses the response (network failure, timeout, browser crash) it
can retry the exact same request with the same `Idempotency-Key` header; the
server detects the duplicate and returns the original `intent_id` again with
`Idempotent-Replayed: true`, without queueing a second intent.

Rules:

- The key is mandatory (missing or invalid values return
  `400 code=idempotency-key-required` / `400 code=invalid-idempotency-key`).
- Keys are scoped to `author_public_key + Idempotency-Key`; the same key from
  a different author is independent.
- The request fingerprint is `METHOD\npath\nsha256(body)`. Reusing a key with
  a different request returns `409 code=idempotency-key-reused`; the conflicting
  request is not recorded and not queued.
- Invalid requests (bad PoW, bad signature, not found, unauthorized, invalid
  JSON) do not consume the key.
- Records are kept for 24 hours, aligned with Stripe's idempotency retention;
  after that the key can be reused.

Clients should generate a fresh key per logical write (e.g. `crypto.randomUUID()`)
and reuse that exact key when retrying the same request. Use the same endpoint
form (path-based or body-based) for all retries of a key.

## Real-time updates (SSE)

`GET /api/v1/sites/{site_id}/posts/{post_slug}/sse`

Server-sent events use the shape `{ "type": "...", "payload": { ... } }`:

```text
type: message_created
type: message_updated
type: message_deleted
type: ephemeral
```

The `message_created` and `message_updated` payloads contain the full
`Message` object (same shape as the list response); `message_deleted`
contains the deleted `event_id` and, when the deletion went through the
Cumments API, the `intent_id`. `ephemeral` carries live room state such as
typing indicators:

```json
{ "type": "typing", "room_id": "!room:server", "user_id": "@alice:server", "typing": true, "display_name": "Alice" }
```

Typing events also arrive as an initial snapshot on connect. Read receipts
and presence are forwarded when the homeserver exposes them, but the demo
only renders typing.

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

`QUERY /api/v1/admin/sites`

Optional JSON body (an empty body means default pagination):

```json
{ "page": 1, "per_page": 20, "site_id": "my-blog" }
```

Returns every database-tracked site merged with the operator-declared
`[sites]` overlay. Each origin carries a `source` of `"verified"` or
`"config"`. Pagination and filtering use `page`, `per_page` (1-100, default
20) and optional `site_id` from the body; the response shape is
`{ "data": [...], "meta": { "total", "page", "per_page", "total_pages" } }`.

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

### List quarantined rooms

`QUERY /api/v1/admin/rooms/quarantined`

Optional JSON body with the same `page` / `per_page` / `site_id` fields as
[List sites](#list-sites).

Returns rooms whose adoption failed governance checks and are currently
quarantined, with the room id, site/post, quarantine reason, when the room
was first quarantined, how many consecutive adoption attempts failed, and
when the next automatic retry is scheduled (`null` means manual attention is
required). Quarantined rooms are retried on a 1h/6h/24h schedule; after the
fourth consecutive failure they require `reinstate`. A successful
re-registration clears the quarantine automatically. The same
pagination/filter fields and `{ "data", "meta" }` shape apply.

### Reinstate a room

`DELETE /api/v1/admin/rooms/quarantined/{room_id}`

Clears a room's quarantine and makes it the canonical room again (any other
active room for the same post is superseded). The operation is idempotent:
reinstating an already-active room also returns `204`; an unknown room
returns `404`.

The old `/api/v1/admin/rooms/blocked` paths are kept as deprecated aliases
for one release.

### Media proxy

`GET /api/v1/media/{server}/{media_id}?expires=...&sig=...`

Public read-only proxy for Matrix media referenced by messages. Message
payloads carry signed proxy URLs (instead of raw `mxc://` URIs) in
`content.url` (media) and `content.thumbnail_url` (media/location); the
signature is an HMAC over
`server/media_id/expires` and expires after 15 minutes. Requests are rate
limited, restricted to the configured homeserver, size-capped, and filtered
by content type. The optional `thumbnail=1` query serves a 320×320
thumbnail.

### Guest media upload

`POST /api/v1/sites/{site_id}/posts/{post_slug}/media?mime=...&filename=...&author_public_key=...&author_signature=...&challenge_response=...`

Uploads raw image/audio bytes as the guest's virtual user and returns
`{ "url", "filename", "mimetype", "size", "voice" }` with an `mxc://` URL.
The signature covers
`["UPLOAD", site_id, post_slug, mime, filename, size, challenge]`;
the upload is rate limited and size/type capped. The returned `url` is then
used in a POST comment request with `media` (the signature covers the media
URL instead of text content).

### Preset stickers

`GET /api/v1/sites/{site_id}/posts/{post_slug}/stickers`

Returns the deployment's preset stickers as
`[{ "url", "proxy_url", "alt" }]` (`url` is the `mxc://` reference used when
posting, `proxy_url` is the signed preview URL). Guests send a sticker by
posting a comment with `media.kind = "sticker"` referencing one of these
`url` values; the API rejects stickers outside the preset list.

### React to a comment

`POST /api/v1/sites/{site_id}/posts/{post_slug}/comments/{comment_id}/reactions`

Body: `{ "key", "author_public_key", "author_signature", "challenge_response" }`.
The signature covers `["REACT", site_id, post_slug, comment_id, key, challenge]`;
the reaction is sent as the guest's virtual user (`m.reaction` with the
signed proof block) and projected into the message's reaction counts.

### Vote on a poll

`POST /api/v1/sites/{site_id}/posts/{post_slug}/polls/{poll_id}/votes`

Body: `{ "option_id", "author_public_key", "author_signature", "challenge_response" }`.
The signature covers `["VOTE", site_id, post_slug, poll_id, option_id, challenge]`;
the vote is sent as `m.poll.response` (MSC3381) with the signed proof block
and aggregated into the poll's response counts.

### Post a location

`POST /api/v1/sites/{site_id}/posts/{post_slug}/location`

Body: `{ "geo_uri", "description?", "author_public_key", "author_signature", "challenge_response" }`.
The signature covers `["LOCATE", site_id, post_slug, geo_uri, challenge]`;
the message is sent as `m.location` (MSC3488) with the signed proof block.

### Room info

`GET /api/v1/sites/{site_id}/posts/{post_slug}/room`

Returns the comment room's current metadata (`name`, `topic`, `avatar_url`,
`member_count`) and the most recent system messages (member joins/leaves,
room name/topic/avatar changes). `avatar_url` is a signed media-proxy URL
when the proxy is enabled.

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
registry is documented in [Problem types](problems/index.md).

## Rate limiting

`POST /api/v1/sites` and `POST /api/v1/sites/{site_id}/verifications` are
rate limited per client IP (10/hour and 20/hour). Limit exceeded returns
`429 code=rate-limited`. Verification `confirm` is limited to 30/hour, comment
writes (`POST`/`PATCH`/`DELETE`) to 120/hour, and new SSE connections to
20/hour with a global cap of 500 concurrent streams.
SSE reconnects within 30 seconds of a disconnect do not consume the hourly
new-connection budget (bounded to 20 free reconnects per client per 5-minute
window), so EventSource auto-reconnect and normal page refreshes do not
silently exhaust the quota.

Client keys are the peer IP by default. `X-Forwarded-For` is honored only
when the peer is listed in `server.trusted_proxies`; the first value is then
used as the client key.

Verification origins must be public by default: loopback/private/link-local
IP-literal origins are rejected unless
`security.allow_private_verification_origins = true`. Each verification
token allows at most 5 confirm attempts before a new challenge is required.

## Validation

`site_id` and `post_slug` accept lowercase `[a-z0-9-]`, 1–64 characters.
Invalid values return `400 code=validation-error`.

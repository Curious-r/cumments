# Visitors

Public self-service reads and profile mutations for visitor identities on a site.

## Get the visitor's current profile

`GET /api/v1/sites/{site_id}/visitors/profile?author_public_key=...`

Returns the visitor's current global Matrix profile for this site. The virtual
user is derived from `site_id + author_public_key`, so this endpoint answers
"who am I on this site?" for a browser-held key without any session.

Response:

```json
{
  "visitor_id": "a1b2c3d4e5f60718a1b2c3d4e5f60718",
  "display_name": "Alice",
  "avatar_url": "https://comments.example.net/api/v1/media/..."
}
```

- `display_name` is the current profile display name, or `null` when unset.
- `avatar_url` is a signed proxy URL (96×96 crop variant when the media proxy is enabled), or `null` when unset or when the media proxy is disabled. Raw `mxc://` transport addresses are never exposed.
- Unknown virtual users and homeservers configured not to disclose profiles
  (`403`, MSC4170) both return an **empty profile** (`null` fields) with
  `200`, so clients treat "no profile" as a normal state.
- Reading profiles is strictly read-only and performs no database writes.

The endpoint is public: the Ed25519 public key is the identity, it is high-entropy and not enumerable. Requests are rate limited per client IP (default 120/hour, configurable via `rate_limit.visitor_profile`).

Errors: `404` when the site is not registered, `400` for missing/invalid `author_public_key`, `429` when rate limited.

## Profile mutations

Mutations to visitor profiles operate on dedicated field endpoints with same-field serialization and atomic idempotency claims. All mutation endpoints require the `Idempotency-Key` header and accept JSON payloads signed by the visitor's Ed25519 key.

### Set visitor display name

`PUT /api/v1/sites/{site_id}/visitors/profile/display_name`

Headers: `Idempotency-Key: <key>`

Body:
```json
{
  "display_name": "Alice",
  "author_public_key": "<base64url-public-key>",
  "author_signature": "<base64url-signature>",
  "challenge_response": "<prefix|nonce>"
}
```

The author signature covers:
`["SET_DISPLAY_NAME", site_id, operation_id, semantic_fingerprint]`

### Clear visitor display name

`DELETE /api/v1/sites/{site_id}/visitors/profile/display_name`

Headers: `Idempotency-Key: <key>`

Body:
```json
{
  "author_public_key": "<base64url-public-key>",
  "author_signature": "<base64url-signature>",
  "challenge_response": "<prefix|nonce>"
}
```

The author signature covers:
`["CLEAR_DISPLAY_NAME", site_id, operation_id, semantic_fingerprint]`

### Set visitor avatar

`PUT /api/v1/sites/{site_id}/visitors/profile/avatar`

Headers: `Idempotency-Key: <key>`

Body:
```json
{
  "avatar": "mxc://hs/550e8400e29b41d4a716446655440000",
  "author_public_key": "<base64url-public-key>",
  "author_signature": "<base64url-signature>",
  "challenge_response": "<prefix|nonce>"
}
```

`avatar` must be an MXC URI (`mxc://server/media_id`) returned by
`POST /api/v1/sites/{site_id}/visitors/profile/avatar/media` for the same
visitor and site. Before the operation is claimed, the API checks that the MXC
has site-scoped avatar-upload provenance: an MXC that is not an `mxc://` URI,
was never uploaded through the avatar path, came through the page-scoped
comment-media upload, belongs to another visitor, or belongs to another site is
rejected with `400`.

The author signature covers:
`["SET_AVATAR", site_id, operation_id, semantic_fingerprint]`

### Clear visitor avatar

`DELETE /api/v1/sites/{site_id}/visitors/profile/avatar`

Headers: `Idempotency-Key: <key>`

Body:
```json
{
  "author_public_key": "<base64url-public-key>",
  "author_signature": "<base64url-signature>",
  "challenge_response": "<prefix|nonce>"
}
```

The author signature covers:
`["CLEAR_AVATAR", site_id, operation_id, semantic_fingerprint]`

### Mutation responses

Successful mutations return the lifecycle state of the operation:

```json
{
  "operation_id": "...",
  "status": "completed",
  "field": "display_name",
  "value": "Alice",
  "error": null
}
```

- `200 OK`: Operation completed synchronously downstream on Matrix.
- `202 Accepted`: Operation accepted and pending execution, dispatching, or in an ambiguous/unknown downstream state.
- Replayed requests return the existing operation state with `Idempotent-Replayed: true`.
- Replaying with a different semantic fingerprint for the same `Idempotency-Key` returns `409 Conflict`.
- Deterministic downstream failures return RFC 9457 problem details error responses.

# API

## Challenge

`GET /api/challenge`

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

`QUERY /api/sites/{site_id}/posts/{post_slug}/comments` (RFC 10008)

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

`POST /api/sites/{site_id}/posts/{post_slug}/comments`

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

`reply_to` is the Matrix event ID of the parent comment, or an empty line when
the comment is not a reply.

### Edit a comment

`PATCH /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

Signature message:

```text
PATCH\n{site_id}\n{post_slug}\n{comment_id}\n{content}\n{challenge_prefix}
```

### Delete a comment

`DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

Signature message:

```text
DELETE\n{site_id}\n{post_slug}\n{comment_id}\n{challenge_prefix}
```

## Real-time updates (SSE)

`GET /api/sites/{site_id}/posts/{post_slug}/sse`

Server-sent events use the shape `{ "type": "...", "payload": { ... } }`:

```text
type: comment_created
type: comment_updated
type: comment_deleted
```

The `comment_created` and `comment_updated` payloads contain the full `Comment`
object; `comment_deleted` contains the deleted `event_id`.

## Validation

`site_id` and `post_slug` accept lowercase `[a-z0-9-]`, 1–64 characters.
Invalid values return `400 VALIDATION_ERROR`.

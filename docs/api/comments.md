# Comments

Comment writes are gated by site authentication: either the browser
`Origin` must match the site's verified/configured origins, or (secret mode)
the request must carry `X-Cumments-Timestamp` and `X-Cumments-Signature`
(HMAC-SHA256 over `timestamp\nMETHOD\npath\nsha256(body)`, ±5 minutes).
See [Site trust](../site-trust.md) for the policy.

Every write also carries the author proof described in the
[API overview](index.md#authors) and an
[`Idempotency-Key`](index.md#idempotent-writes) header.

## List comments

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
      "edited_at": null,
      "reply_to": null,
      "thread_root": null,
      "submission_id": 42,
      "status": "active",
      "redacted_at": null,
      "redacted_by": null,
      "reactions": [
        { "key": "👍", "count": 2 }
      ]
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

Media URLs and author avatars (`author.avatar_url`) are rewritten to signed
proxy URLs when the media proxy is enabled (avatars through the 96×96 crop
variant); see [Media proxy](media.md#media-proxy).

`author.display_name` and `author.avatar_url` render the author's **current**
joined `m.room.member` profile: renaming or changing the avatar updates old
comments as well. The value captured at projection time is only used as a
fallback after the author leaves the room.

## Post a comment

`POST /api/v1/sites/{site_id}/posts/{post_slug}/comments`

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
[Guest media upload](media.md#guest-media-upload), or a site sticker pack
reference with `"kind": "sticker"`), the signature covers `media.url` instead
of `content`; `content` then only serves as the fallback filename/text.

Successful writes are asynchronous and return `202` with the queue row ID:

```json
{ "submission_id": 42 }
```

The `202` acknowledges that the submission is durably stored in the local
queue; it does not mean the comment exists in Matrix yet. See
[Idempotent writes](index.md#idempotent-writes) for the exact durability
semantics.

The projected comment (list/SSE `message_created`) carries the same
`submission_id` when it was submitted through the Cumments API, so clients can
correlate the accepted request with the final comment. Matrix-native comments
omit `submission_id`.

Signature message:

```text
POST\n{site_id}\n{post_slug}\n{content}\n{reply_to}\n{challenge_prefix}
```

`reply_to` is the exact Matrix event ID of the parent comment as returned by
the API, or an empty line when the comment is not a reply. Event IDs are
opaque strings by spec; legacy v1/v2 IDs look like `$localpart:server` while
room v3+ IDs are bare hashes (v3 may even contain `/`). When an event ID is
used in a request path (the path-based edit form) or query string (delete),
clients must percent-encode it.

`display_name` is presentation data and is deliberately **not** part of the
signature: the API writes it to the virtual user's Matrix profile, and the
event proof block only carries `public_key`, `signature`, `challenge`,
`content` and `submission_id`.

## Edit a comment

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

Both edit forms are supported: the body-based form avoids percent-encoding
opaque event IDs, while the path-based form keeps the target in the URL.
Both require the `Idempotency-Key` header.

## Delete a comment

`DELETE /api/v1/sites/{site_id}/posts/{post_slug}/comments?comment_id=$event%3Aserver`

The target event id travels as a percent-encoded `comment_id` query
parameter. RFC 9110 leaves DELETE request bodies undefined, so Cumments
never puts the target in a DELETE body — that keeps requests acceptable to
proxies that reject body-bearing DELETEs. The body carries only the author
proof:

```json
{
  "author_public_key": "...",
  "author_signature": "...",
  "challenge_response": "challenge|nonce"
}
```

Signature message:

```text
DELETE\n{site_id}\n{post_slug}\n{comment_id}\n{challenge_prefix}
```

The request requires the `Idempotency-Key` header.

## React to a comment

`POST /api/v1/sites/{site_id}/posts/{post_slug}/comments/{comment_id}/reactions`

Body: `{ "key", "author_public_key", "author_signature", "challenge_response" }`.
The signature covers `["REACT", site_id, post_slug, comment_id, key, challenge]`;
the reaction is sent as the guest's virtual user (`m.reaction` with the
signed proof block) and projected into the message's reaction counts.

## Vote on a poll

`POST /api/v1/sites/{site_id}/posts/{post_slug}/polls/{poll_id}/votes`

Body: `{ "option_id", "author_public_key", "author_signature", "challenge_response" }`.
The signature covers `["VOTE", site_id, post_slug, poll_id, option_id, challenge]`;
the vote is sent as `m.poll.response` (MSC3381) with the signed proof block
and aggregated into the poll's response counts.

## Post a location

`POST /api/v1/sites/{site_id}/posts/{post_slug}/location`

Body: `{ "geo_uri", "description?", "display_name", "author_public_key", "author_signature", "challenge_response" }`.
The signature covers
`["LOCATE", site_id, post_slug, geo_uri, challenge]`;
the message is queued like a comment (same `Idempotency-Key` and
`202 { "submission_id" }` contract) and sent as `m.location` (MSC3488) with the
signed proof block, closing the loop through the same projection path.
As with comments, `display_name` is written to the virtual user's profile and
not covered by the signature.

## Room info

`GET /api/v1/sites/{site_id}/posts/{post_slug}/room`

Returns the comment room's current metadata (`name`, `topic`, `avatar_url`,
`avatar_thumbnail_url`, `member_count`) and the most recent system messages
(member joins/leaves, room name/topic/avatar changes). `avatar_url` is a
signed media-proxy URL and `avatar_thumbnail_url` is the same image through
the 96×96 crop variant, both when the proxy is enabled. See
[Data model](../data-model.md) for the room metadata tables.

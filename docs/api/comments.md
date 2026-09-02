# Comments

Length limits for user-facing text are measured in Unicode extended grapheme clusters according to UAX #29, not UTF-8 bytes, Unicode scalar values, or UTF-16 code units.

Comment writes are gated by site authentication: either the browser
`Origin` must match the site's verified/configured origins, or (secret mode)
the request must carry `X-Cumments-Timestamp` and `X-Cumments-Signature`
(HMAC-SHA256 over `timestamp\nMETHOD\npath\nsha256(body)`, ±5 minutes).
See [Site trust](../site-trust.md) for the policy.

Every write carries the author proof described in the
[API overview](index.md#authors). Durable submission endpoints also require an
[`Idempotency-Key`](index.md#idempotent-writes) header; reactions, poll votes,
and visitor avatar deletion are natural-idempotent exceptions described below.

## List comments

`QUERY /api/v1/sites/{site_id}/pages/{page_slug}/comments` (RFC 10008)

Body:

```json
{ "page": 1, "per_page": 20 }
```

Optional personalization — when `author_public_key` and `author_signature` are supplied and verify against `["QUERY_COMMENTS", site_id, page_slug]`, each `ReactionSummary` gains `mine: true` for keys the requesting virtual user reacted with (derived view, never stored). Anonymous reads return `mine: false`:

```json
{
  "page": 1,
  "per_page": 20,
  "author_public_key": "...",
  "author_signature": "..."
}
```

Response:

```json
{
  "data": [
    {
      "event_id": "$event:server",
      "site_id": "my-blog",
      "page_slug": "hello-world",
      "author": {
        "type": "visitor",
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
        { "key": "👍", "count": 2, "mine": false }
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
- `redacted`: `{ "type": "redacted" }`
- `encrypted`: `{ "type": "encrypted", "algorithm": "m.megolm.v1.aes-sha2", "sender_key": … }`
- `unknown`: `{ "type": "unknown", "fallback": …, "raw": { … } }`

Media URLs and author avatars (`author.avatar_url`) are rewritten to signed
proxy URLs when the media proxy is enabled (avatars through the 96×96 crop
variant); see [Media proxy](media.md#media-proxy).

A redacted comment remains as a tombstone with `status: "redacted"` and
`content: {"type": "redacted"}`. It does not expose the original body, media,
relations or raw Matrix payload. Edits, deletes, replies, reactions and poll
votes cannot target it.

`author.display_name` and `author.avatar_url` render the author's **current**
joined `m.room.member` profile: renaming or changing the avatar updates old
comments as well. The value captured at projection time is only used as a
fallback after the author leaves the room.

## Post a comment

`POST /api/v1/sites/{site_id}/pages/{page_slug}/comments`

Body:

```json
{
  "content": "...",
  "media": null,
  "display_name": "Alice",
  "author_public_key": "...",
  "author_signature": "...",
  "reply_to": null,
  "thread_root": null,
  "challenge_response": "challenge|nonce"
}
```

When `media` is present (an object returned by
[Visitor media upload](media.md#visitor-media-upload), or a site sticker pack
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

Signature message (JSON array, `null` for absent relations):

```json
["POST","{site_id}","{page_slug}","{content}",reply_to,thread_root,"{challenge_prefix}","1"]
```

`reply_to` is the parent for `m.in_reply_to`; `thread_root` is the root for
`m.thread` (`rel_type: "m.thread"`). Both are `null` when absent, so the
type distinguishes missing from empty. Either may be any Matrix event ID as
returned by the API (`reply_to` and `thread_root` are orthogonal and may be
present together — Matrix encodes both in the same `m.relates_to`). Event IDs
are opaque strings by spec; legacy v1/v2 IDs look like `$localpart:server`
while room v3+ IDs are bare hashes (v3 may even contain `/`). When an event ID
is used in a request path (the path-based edit form) or query string (delete),
clients must percent-encode it.

`display_name` is presentation data and is deliberately **not** part of the
signature: the API writes it to the virtual user's Matrix profile, and the
event proof block only carries `public_key`, `signature`, `challenge`,
`content` and `submission_id`.

## Edit a comment

`PATCH /api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}`

Signature message:

```json
["PATCH","{site_id}","{page_slug}","{comment_id}","{content}","{challenge_prefix}","1"]
```

The request requires the `Idempotency-Key` header. Event IDs are opaque, so
clients must percent-encode `comment_id`.

## Delete a comment

`DELETE /api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}`

The body carries only the author proof:

```json
{
  "author_public_key": "...",
  "author_signature": "...",
  "challenge_response": "challenge|nonce"
}
```

Signature message:

```json
["DELETE","{site_id}","{page_slug}","{comment_id}","{challenge_prefix}"]
```

The request requires the `Idempotency-Key` header. Event IDs are opaque, so
clients must percent-encode `comment_id`.

## React to a comment

`POST /api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}/reactions`

Body: `{ "key", "author_public_key", "author_signature", "challenge_response" }`.
The signature covers `["REACT", site_id, page_slug, comment_id, key, challenge, "1"]`;
`key` is the reaction key (emoji, 1-32 Unicode extended grapheme clusters per UAX #29, trimmed, no control characters);
duplicate annotations from the same virtual user are treated as idempotent
(`M_DUPLICATE_ANNOTATION` maps to `204`). The reaction is sent as the visitor's
virtual user (`m.reaction` with the signed proof block) and projected into the
message's reaction counts.
This endpoint does not use `Idempotency-Key`. Matrix uses a deterministic
transaction ID derived from the signed request and PoW challenge, so retrying
the exact same Matrix request does not create another aggregate reaction. The
PoW challenge is single-use at the HTTP API boundary, however, so a repeated
HTTP request after success returns invalid-PoW instead of duplicating the
effect. Percent-encode `comment_id` in the path.

## Remove a reaction

`DELETE /api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}/reactions/{key}`

Body: `{ "author_public_key", "author_signature", "challenge_response" }`.
The signature covers `["UNREACT", site_id, page_slug, comment_id, key, challenge]`;
`key` is the normalized reaction key from the path (trimmed, no control
characters, 1-32 Unicode extended grapheme clusters per UAX #29). The reaction is resolved by
`(comment, virtual user derived from author_public_key, key)` without
exposing Matrix event IDs and redacted via the AS sender. This endpoint does
not use `Idempotency-Key`; it is natural-idempotent (`204` even when the
reaction is already absent, `M_NOT_FOUND` on the homeserver is treated as
success). Matrix uses a deterministic transaction ID derived from the signed
request and PoW challenge. Percent-encode `comment_id` and `key` (emoji) in the path.

## Create a poll

`POST /api/v1/sites/{site_id}/pages/{page_slug}/polls`

Body:

```json
{
  "question": "Which language do you prefer?",
  "options": ["Rust", "TypeScript", "Python"],
  "max_selections": 1,
  "display_name": "Alice",
  "author_public_key": "...",
  "author_signature": "...",
  "reply_to": null,
  "thread_root": null,
  "challenge_response": "challenge|nonce"
}
```

* `question` — 1–500 Unicode extended grapheme clusters per UAX #29, no leading/trailing whitespace, no control characters.
* `options` — 2–20 ordered option texts, each 1–200 Unicode extended grapheme clusters per UAX #29, no leading/trailing whitespace, no control characters.
* `max_selections` — optional, defaults to `1`; only `1` is accepted (single-select). The current authoring API is single-select even though MSC3381 supports multi-select; the wire format preserves the declared limit.
* `display_name` — presentation data written to the virtual user's Matrix profile, not covered by the signature.
* `reply_to` / `thread_root` — orthogonal reply/thread relations, `null` when absent, same model as comment posts; Matrix encodes both in `m.relates_to`.

Successful writes are asynchronous and return `202` with the queue row ID:

```json
{ "submission_id": 42 }
```

The request requires the `Idempotency-Key` header and is durable: it is queued as a `PostCommentCommand { poll: Some(...) }` through the existing `PendingPostSubmission` pipeline (`save_post_submission_idempotent` → `PostsPass` → `MatrixDriver::post_poll` → `m.poll.start`), sharing the same transaction-ID, retry, `waiting_for_sync`, and idempotency semantics as comments and locations.

Signature message (JSON array, `null` for absent relations):

```json
["POLL","{site_id}","{page_slug}","{canonical_poll_payload}",reply_to,thread_root,"{challenge_prefix}","1"]
```

where `canonical_poll_payload` is the deterministic JSON string

```json
{"question":"...","options":["...","..."],"max_selections":1}
```

with ordered `options`. Any change to `question`, option text, option order, or `max_selections` invalidates the signature; `display_name` is not signed.

The Matrix event is `m.room.message` with `msgtype: "org.matrix.msc3381.poll.start"` and `org.matrix.msc3381.poll.start` containing `question`, `answers` with deterministic IDs `"0"`, `"1"`, …, and `max_selections`. The fallback `body` is the question followed by a numbered option list. Reply/thread relations are emitted as `m.relates_to`.

## Vote on a poll

`POST /api/v1/sites/{site_id}/pages/{page_slug}/polls/{poll_id}/votes`

Body: `{ "option_id", "author_public_key", "author_signature", "challenge_response" }`.
The signature covers `["VOTE", site_id, page_slug, poll_id, option_id, challenge, "1"]`;
the vote is sent as `m.poll.response` (MSC3381) with the signed proof block
and aggregated into the poll's response counts.
This endpoint does not use `Idempotency-Key`. Matrix uses a deterministic
transaction ID derived from the signed request and PoW challenge, so retrying
the exact same Matrix request does not create another vote event. The PoW
challenge is single-use at the HTTP API boundary, however, so a repeated HTTP
request after success returns invalid-PoW instead of duplicating the effect.

## Post a location

`POST /api/v1/sites/{site_id}/pages/{page_slug}/location`

Body: `{ "geo_uri", "description?", "display_name", "author_public_key", "author_signature", "challenge_response", "reply_to?", "thread_root?" }`.
The signature covers
`["LOCATE", site_id, page_slug, geo_uri, reply_to, thread_root, challenge, "1"]`
(`reply_to` / `thread_root` orthogonal, `null` when absent — same model as
comment posts; Matrix encodes both in `m.relates_to`);
the message is queued like a comment (same `Idempotency-Key` and
`202 { "submission_id" }` contract) and sent as `m.location` (MSC3488) with the
signed proof block, closing the loop through the same projection path.
As with comments, `display_name` is written to the virtual user's profile and
not covered by the signature.

## Room info

`GET /api/v1/sites/{site_id}/pages/{page_slug}/room`

Returns the comment room's current metadata (`name`, `topic`, `avatar_url`,
`avatar_thumbnail_url`, `member_count`) and the most recent system messages
(member joins/leaves, room name/topic/avatar changes). `avatar_url` is a
signed media-proxy URL and `avatar_thumbnail_url` is the same image through
the 96×96 crop variant, both when the proxy is enabled. See
[Data model](../data-model.md) for the room metadata tables.

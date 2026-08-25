# Data model

This page describes how Matrix events are shaped into the typed comment model
that the API and SSE stream expose. It documents the mapping and the storage
layout; the runtime system around them is covered in
[architecture](architecture.md).

The read model separates Matrix-derived facts from local control state. Fact
projection rows can be rebuilt from Matrix history with `cumments backfill`;
submission queues, idempotency records, claims, secrets, audit entries,
quarantine decisions and anti-resurrection tombstones are local control-plane
state with their own durability rules (see
[architecture](architecture.md)).

## Design principles

1. **Content is a sealed type system.** Each displayable kind of content has
   its own structured payload instead of one universal JSON blob. Clients
   dispatch on the `type` tag.
2. **Raw JSON is the escape hatch.** The protocol keeps evolving; unknown or
   future types keep their original payload so history is never lost and can
   be reinterpreted later.
3. **Relations are not content.** Replies, edits, threads, reactions and
   deletions are attributes or edges of a message, modeled separately from
   the body.
   Public reads hide a reply/thread edge when its target is deleted or no
   longer visible; the child remains an ordinary comment.
4. **Authors follow live profiles.** Display name and avatar render from the
   author's current joined `m.room.member` profile, so renames and avatar
   changes propagate to old comments. The values captured at projection time
   are kept as a fallback for authors who left the room.
5. **Boundaries are explicit.** Voice/video calls are out of scope, and
   encrypted content is modeled only as a placeholder.

## Message shape

```rust
Message {
    event_id,               // the Matrix event ID
    site_id, page_slug,     // which site and page this comment belongs to
    author: AuthorSnapshot, // visitor (public_key) or matrix (mxid)
    content: Content,       // sealed enum, see below
    timestamp,              // Matrix origin_server_ts
    edited_at,              // last m.replace timestamp, if any
    reply_to, thread_root,  // relation targets, if any
    submission_id,              // set when submitted through the API
    status,                 // active | redacted
    redacted_at, redacted_by,
    reactions: [ReactionSummary],
}
```

Edit revisions are stored in a dedicated `message_revisions` table, but the API
currently exposes only `edited_at`. A revision is an immutable relation fact;
redacting an edit hides that revision and the displayed content falls back to
the latest surviving revision or the original message.
Redaction also replaces the revision payload with a redacted tombstone; only
its event metadata remains for replay/audit.
`room_id`, `sender_mxid` and the raw Matrix `content` are internal integrity
fields and are never serialized to API or SSE clients.

Visitor authors carry an Ed25519 `public_key`; their virtual-user Matrix ID is
an implementation detail and is not exposed. Matrix-native authors carry
their `mxid` and never a `public_key`.

### Content types

| Type | Payload |
|---|---|
| `text` | `body`, optional HTML `formatted_body`, `style` (`normal`/`emote`/`notice`) |
| `media` | `kind` (image/video/audio/file/sticker), `url`, optional filename, mimetype, size, dimensions, thumbnail, alt text, `voice` flag |
| `location` | `geo_uri`, optional description and thumbnail |
| `poll` | `question`, `options`, aggregated `responses` |
| `redacted` | Stable empty tombstone; no original body, URL, relations or raw payload |
| `encrypted` | algorithm and sender key placeholder only |
| `unknown` | optional `fallback` text plus the original raw JSON |

## Matrix event mapping

| Matrix event | Model result |
|---|---|
| `m.room.message` with `m.text` / `m.notice` / `m.emote` | `Content::Text` |
| `m.image` | `Content::Media(Image)` |
| `m.video` / `m.audio` (with the MSC3245 voice flag) | `Content::Media(Video/Audio)` |
| `m.file` | `Content::Media(File)` |
| `m.sticker` | `Content::Media(Sticker)` |
| `m.location` (MSC3488) | `Content::Location` |
| `m.poll.start` (MSC3381) | `Content::Poll` |
| `m.poll.response` | Aggregated into `PollContent.responses` |
| `m.reaction` | Reaction annotation, aggregated into `Message.reactions` |
| `m.replace` | Edit; appends a revision and updates `edited_at` |
| `m.room.redaction` | `status = redacted`; content becomes `{"type":"redacted"}` |
| `m.room.encrypted` | `Content::Encrypted` placeholder |
| Anything else | `Content::Unknown` with the raw payload kept |

Reactions and poll responses are annotation edges, not comment messages: they
are stored in their own tables and aggregated onto the target message.

## Avatars

Matrix has no single avatar entity; avatars live in three spec-defined
places and Cumments projects all of them:

- **Global profile** (`avatar_url` profile field): the canonical identity
  avatar of a user. Visitors set it through the visitor avatar API, which stores
  it on the virtual user's profile and propagates it to joined rooms as
  `m.room.member` events (MSC4466 `propagate_to: all` query parameter).
- **`m.room.member.avatar_url`**: the per-room profile snapshot. It is the
  source used when projecting message authors; leave events keep the last
  known value instead of wiping the snapshot, and redaction removes it.
- **`m.room.avatar`**: the room's own avatar. `url` absent means "no
  avatar"; `info.thumbnail_url` is preserved in the raw state JSON but is
  not part of the API contract — the room endpoint derives
  `avatar_thumbnail_url` from the main image through the 96×96 crop
  thumbnail variant instead.

Author profiles are projected into `author_display_name` / `author_avatar_url`
when a message is stored, but the public read path (message list, single
message, SSE) overlays the author's current joined `m.room.member` profile.
Visitors and Matrix-native authors behave identically: display data is Matrix
profile state, never signed event content (see the
[demo frontend](demo.md)). Members who
left the room keep the stored projection as a fallback; a redacted member
state keeps the membership but drops the profile, so reads show no profile,
same as a cleared display name or avatar.

All avatar URLs are stored as `mxc://` and rewritten to signed media-proxy
URLs on the way out of the API (see [Media proxy](#media-proxy)); the proxy
itself uses the authenticated `/_matrix/client/v1/media` endpoints with the
AppService token.

## Not part of the comment stream

Two classes of events are deliberately excluded from the `Message` model:

- **Ephemeral events** (`m.typing`, receipts, presence) are not part of room
  history and cannot be backfilled; they flow through a separate channel, see
  [Ephemeral events](#ephemeral-events).
- **Room state events** (`m.room.member`, name, topic, avatar,
  `m.room.power_levels`, tombstones, `m.space.*`) *are* in room history, but
  they are room metadata rather than comments. A light metadata model
  (`room_members` + `room_state_events`) records joins/leaves and name,
  topic and avatar changes; `GET .../room` returns that metadata (including
  `avatar_thumbnail_url`) and the most recent system messages. Power levels
  feed the governance projection instead (see
  [site governance](site-governance.md)).

Voice/video calls are not modeled at all.

## Storage

```sql
messages (
  event_id TEXT PRIMARY KEY,
  room_id, site_id, page_slug,
  author_type, author_mxid, author_display_name, author_avatar_url,
  author_public_key,
  content_json JSON,          -- the serialized Content enum
  original_content_json JSON, -- displayable content when first projected
  matrix_event_type TEXT,     -- replacement type validation
  raw_content_json JSON,      -- original Matrix content (escape hatch)
  reply_to_event_id, thread_root_event_id,
  timestamp, edited_at,
  status, redacted_at, redacted_by,
  submission_id
)

message_revisions (
  message_id, event_id PK, content_json, edited_at, editor,
  redacted_at, redacted_by
)

reactions (event_id UNIQUE, message_event_id, sender_mxid, key, timestamp, redacted_at)

poll_response_events (
  event_id UNIQUE, poll_message_id, sender_mxid,
  option_index, timestamp, redacted_at, redacted_by
)
```

Notes on the layout:

- Edit revisions include redaction metadata so removing one replacement can
  roll the public view back deterministically. Redaction clears the authored
  replacement payload.
- Parent deletion sanitizes the parent and removes all of its revisions; late
  replacements cannot restore deleted content.
- Deleting a comment also clears its original/current payloads and raw Matrix
  content.
- The author proof (`signature`, `challenge`) is verified at projection time
  and is **not** stored in the read model; only the public key is kept for
  edit/delete authorization.
- `reactions` and `poll_response_events` are keyed by event ID, making push
  redelivery and backfill idempotent. Poll aggregation selects each voter's
  latest non-redacted response; redacting that response clears its selected
  option and restores the previous valid vote.
- `formatted_body` is passed through unchanged. The demo renders plain text
  only; any client rendering HTML must sanitize it first.
- `media_uploads.page_slug` is nullable: comment media records the page it
  was authorized for, while visitor avatars are site-scoped records with a
  `NULL` page. Avatar media is marked referenced at upload time so the
  unused-media sweep never collects a profile avatar.

## Ephemeral events

Typing indicators, read receipts and presence are not comments, but they make
the comment section feel alive. Cumments keeps a resident `/sync` connection
with the AppService token and bot identity (the push transaction stream does
not carry ephemeral events), filters for ephemeral/presence only, and keeps
per-room in-memory state:

- Subscribing SSE clients receive the current snapshot first, then
  incremental `ephemeral` events (`type: typing` / `receipt` / `presence`).
- Presence is filtered to users who are joined members of a subscribed room,
  so it cannot leak across sites; the typing snapshot carries the member's
  display name from `room_members`.
- Nothing is persisted or projected; it is an in-memory event channel.
- Protocol limits still apply: private read receipts (MSC2285) are invisible
  to the AppService, so only public receipts (MSC2666) can be surfaced.

## Visitor sending capability

The API turns typed requests into Matrix events sent by each visitor's virtual
user. Every event carries a signed proof block under the
`host.curious.cumments` content namespace, which the projector verifies
before trusting the projection.

| Content kind | Visitor sending | Mechanism |
|---|---|---|
| Text | Supported | `m.text` with reply/edit/delete, queued as a submission |
| Image / video / audio / file / voice | Supported | Upload endpoint → virtual-user Matrix upload → `mxc://` reference in the message; orphaned uploads are garbage-collected |
| Sticker | Supported | Choose from the site's sticker packs (`m.room.image_pack` on the site Space); the API validates the reference and fills metadata, visitors cannot upload stickers |
| Location | Supported | `m.location` (MSC3488), queued like a comment |
| Poll | Supported | API proxies `m.poll.start` / `m.poll.response` with proof |
| Reaction | Supported | API proxies `m.reaction` with proof; deduplicated per sender + key |
| Encrypted | Excluded | Conflicts with visitor verification, AS proxying and auditing |
| Unknown / arbitrary raw events | Excluded | Visitors may only send the whitelisted typed requests |

Reactions and votes are sent synchronously and are naturally idempotent
(reaction dedupe by sender + key, latest-vote-wins); text, media, location
and other comment-shaped writes go through the async submission queue with an
`Idempotency-Key` (see [API](api/index.md#idempotent-writes)).

## Media proxy

`mxc://` URIs require Matrix credentials to download, so Cumments exposes a
public, read-only proxy with short-lived signed URLs. The proxy is limited
to the configured homeserver, rate limited, size-capped, filtered by content
type, answers media and thumbnail requests through the authenticated
`/_matrix/client/v1/media` endpoints (MSC3916) with the AppService token, and
applies Matrix media-id/CSP/disposition safety rules.
Thumbnail requests carry signed `width`/`height`/`method` parameters;
message thumbnails default to 320×240 `scale` and avatars to 96×96 `crop`.
It is deliberately read-only: site administrators browse media directly in
their Matrix client, which is one benefit of building on Matrix (see
[API](api/media.md#media-proxy)).

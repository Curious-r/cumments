# Media

## Media proxy

`GET /api/v1/media/{server}/{media_id}?expires=...&sig=...` with optional
`width`, `height` and `method=crop|scale` query parameters.

Public read-only proxy for Matrix media referenced by messages. Message
payloads carry signed proxy URLs (instead of raw `mxc://` URIs) in
`content.url` (media) and `content.thumbnail_url` (media/location); the
signature is an HMAC over
`server/media_id/width/height/method/expires` (absent thumbnail parameters
are signed as their defaults) and expires after 15 minutes. Requests are
rate limited, restricted to the configured homeserver, size-capped, and
filtered by content type. Media IDs must follow the Matrix whitelist
(`A-Za-z0-9_-`, at most 255 bytes). When `server.public_base_url` is configured the
URLs are absolute against it; otherwise the base is derived from the request
(`Host`, plus `X-Forwarded-Proto`/`X-Forwarded-Host` from trusted proxies),
so URLs are absolute for each client. They only stay API-relative when the
request carries no usable host.

The proxy mirrors the Matrix thumbnail endpoint's semantics: `width` and
`height` must be provided together and `method` defaults to `scale`; the
homeserver is queried through the authenticated `/_matrix/client/v1/media`
endpoints (MSC3916) with the AppService token. Callers never embed a token
in the URL. Two size presets are used by the API itself:

- message/location thumbnails: 320×240, `scale`;
- avatars: 96×96, `crop` (the spec's recommended avatar bucket).

Responses use a canonical MIME type and a server-generated filename; the
opaque media ID is never placed in `Content-Disposition`. Matrix's inline-safe
MIME types are served with `Content-Disposition: inline`; SVG, PDF,
octet-stream, and uncommon image/video/audio types are served as
`attachment`. All successful media responses include a sandbox CSP,
`X-Content-Type-Options: nosniff`,
`Cross-Origin-Resource-Policy: cross-origin`, and
`Referrer-Policy: no-referrer`.

The proxy is read-side and independent of upload bookkeeping: it translates an
`mxc://` reference into a signed URL and depends only on the MXC and the
homeserver. It does not consult `media_uploads`, upload provenance, or any
media lifecycle state.

## Visitor media upload

`POST /api/v1/sites/{site_id}/pages/{page_slug}/media?mime=...&filename=...&author_public_key=...&author_signature=...&challenge_response=...`

Uploads raw image/video/audio/file bytes as the visitor's virtual user and
returns `{ "url", "filename", "mimetype", "size", "voice" }` with an
`mxc://` URL. The signature covers
`["UPLOAD", site_id, page_slug, mime, filename, sha256_hex(body), challenge]`;
the upload requires the same `Idempotency-Key` header as comment write
submissions and is rate limited and size/type capped. Replays return the
original `mxc://` URL with `Idempotent-Replayed: true` without uploading a
second copy; keys are retained for 24 hours like comment write keys. The
returned `url` is then used in a POST comment request with `media` (the
signature covers the media URL instead of text content).

The upload records an upload-provenance record for the MXC against the
uploading visitor and page. A comment may only reference media recorded for the
same author, site, and page. The same `media_uploads` provenance covers
site-scoped avatar uploads (see below); it is local write admission and does
not own or manage the Matrix media object. Matrix media lifetime — retention
and deletion — belongs to the homeserver, and Cumments never deletes media from
it.

## Visitor avatar upload

`POST /api/v1/sites/{site_id}/visitors/profile/avatar/media?mime=...&filename=...&author_public_key=...&author_signature=...&challenge_response=...`

Site-scoped variant of the media upload for profile avatars. It uploads raw
bytes as the visitor's virtual user and returns the same response shape as the
comment-media upload, with `url` carrying the raw Matrix MXC URI. That MXC is a
write-side intermediate value for the subsequent profile mutation, not a
browser media URL.

The signature covers
`["UPLOAD", site_id, mime, filename, sha256_hex(body), challenge]` — the
site-scoped upload signature deliberately carries no page slug. It shares the
comment-media upload's security model: `Idempotency-Key`, author Ed25519
signature, PoW freshness, write rate limiting, the size cap, and allowed-MIME
validation. Replays return the original MXC without uploading a second copy.

The upload records an upload-provenance record for the MXC against the
uploading visitor and site with no page, which is what authorizes a later
avatar mutation: only an upload made through this path, by the same visitor,
for the same site, can be set as the avatar. A page-scoped comment-media upload
never authorizes an avatar. As with comment media, this is local write
admission and does not own or manage the Matrix media object; Matrix media
lifetime belongs to the homeserver, and Cumments never deletes media from it.

## Visitor avatar

Visitor avatar mutations are managed through a dedicated profile endpoint
operating on the Matrix media URI itself.

To set an avatar:
1. Upload media through
   `POST /api/v1/sites/{site_id}/visitors/profile/avatar/media` to obtain its
   `mxc://` content URI.
2. Submit a `PUT /api/v1/sites/{site_id}/visitors/profile/avatar` request with
   that MXC URI.

The profile mutation verifies that the MXC has site-scoped avatar-upload
provenance for this visitor and site before it claims the durable operation; a
page-scoped upload, another visitor's upload, or another site's upload is
rejected with `400`.

To clear an avatar:
- Submit a `DELETE /api/v1/sites/{site_id}/visitors/profile/avatar` request.
  Clearing does not consult upload provenance.

Reads are separate: `GET /api/v1/sites/{site_id}/visitors/profile` returns the
Matrix avatar MXC rewritten through the media proxy into a signed browser-facing
`avatar_url`; the browser should use that value rather than the raw MXC. See
[Visitors documentation](/api/visitors#set-visitor-avatar) for full request
specifications and signature envelopes.

## Site sticker packs

Sticker packs are Matrix-native `m.room.image_pack` state events on the
site's Space (MSC2545). Site admins and managers manage them in any
Matrix client; the bot commands and the endpoints below are the scripted
equivalents, and the public read endpoint serves the projected packs to
visitors.

`GET /api/v1/sites/{site_id}/stickers` (public)

Returns `{ "packs": [{ "pack_id", "display_name", "avatar_url",
"avatar_proxy_url", "images": [ { "shortcode", "url", "proxy_url", "body",
"info" } ] }] }`. `url` / `avatar_url` are the `mxc://` references;
`proxy_url` / `avatar_proxy_url` are signed preview URLs (cross-server MXC
included, avatars through the 96×96 crop variant). Visitors send a sticker by
posting a comment with `media.kind = "sticker"` referencing one of these
`url` values; the API validates it against the site's packs and fills the
`m.sticker` metadata from the pack.

`POST /api/v1/sites/{site_id}/packs/{pack_id}/stickers` (site governance,
claim token)

Body: `{ "shortcode", "url", "body"?, "info"? }`. Adds or replaces one image
in the pack (creating the pack implicitly). Returns the updated pack.

`DELETE /api/v1/sites/{site_id}/packs/{pack_id}/stickers/{shortcode}`
(site governance, claim token)

Removes one image from the pack. Returns the updated pack; removing a
missing shortcode is a successful no-op.

Operator fallbacks mirror both writes under
`/api/v1/operator/sites/{site_id}/packs/{pack_id}/stickers[/{shortcode}]`
with the operator token.

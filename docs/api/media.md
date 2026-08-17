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
filtered by content type. When `server.public_base_url` is configured the
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

## Visitor avatar

`PUT /api/v1/sites/{site_id}/visitors/avatar?mime=...&filename=...&author_public_key=...&author_signature=...&challenge_response=...`

Uploads raw image bytes as the visitor's virtual user and sets the avatar on
that virtual user's global profile in one request. The signature covers
`["UPLOAD_AVATAR", site_id, mime, sha256_hex(body), challenge]`; `mime` must
be an `image/*` type and the request uses the same `Idempotency-Key` header,
rate limiting, size/type caps and 24-hour replay window as visitor media
uploads. The response returns the avatar as a signed proxy URL
(`{ "avatar_url": "https://.../api/v1/media/..." }`, absolute against the
base resolved as described above; the raw MXC URL when the media proxy is
disabled). Replays return the original URL with
`Idempotent-Replayed: true` and re-apply the profile write so a retry heals
a partially completed request.

The avatar is stored in the virtual user's Matrix profile and propagates to
the rooms the user has joined as `m.room.member` events
(MSC4466 `propagate_to: all` query parameter), so Matrix clients and the Cumments
projection observe it without an event-content fallback. Avatars are
site-scoped: the virtual user is derived from `site_id + author_public_key`,
so the same visitor has independent avatars per site.

`DELETE /api/v1/sites/{site_id}/visitors/avatar?author_public_key=...&author_signature=...&challenge_response=...`

Removes the avatar. The signature covers
`["DELETE_AVATAR", site_id, challenge]`; deleting a missing avatar is a
successful no-op.

## Site sticker packs

Sticker packs are Matrix-native `m.room.image_pack` state events on the
site's Space (MSC2545). Site owners and global-moderators manage them in any
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

`DELETE /api/v1/sites/{site_id}/packs/{pack_id}/stickers?shortcode=...`
(site governance, claim token)

Removes one image from the pack. Returns the updated pack; removing a
missing shortcode is a successful no-op.

Operator fallbacks mirror both writes under
`/api/v1/operator/sites/{site_id}/packs/{pack_id}/stickers` with the
operator token.

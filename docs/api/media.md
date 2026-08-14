# Media

## Media proxy

`GET /api/v1/media/{server}/{media_id}?expires=...&sig=...`

Public read-only proxy for Matrix media referenced by messages. Message
payloads carry signed proxy URLs (instead of raw `mxc://` URIs) in
`content.url` (media) and `content.thumbnail_url` (media/location); the
signature is an HMAC over
`server/media_id/expires` and expires after 15 minutes. Requests are rate
limited, restricted to the configured homeserver, size-capped, and filtered
by content type. The optional `thumbnail=1` query serves a 320×320
thumbnail.

## Guest media upload

`POST /api/v1/sites/{site_id}/posts/{post_slug}/media?mime=...&filename=...&author_public_key=...&author_signature=...&challenge_response=...`

Uploads raw image/video/audio/file bytes as the guest's virtual user and
returns `{ "url", "filename", "mimetype", "size", "voice" }` with an
`mxc://` URL. The signature covers
`["UPLOAD", site_id, post_slug, mime, filename, sha256_hex(body), challenge]`;
the upload requires the same `Idempotency-Key` header as comment write
submissions and is rate limited and size/type capped. Replays return the
original `mxc://` URL with `Idempotent-Replayed: true` without uploading a
second copy; keys are retained for 24 hours like comment write keys. The
returned `url` is then used in a POST comment request with `media` (the
signature covers the media URL instead of text content).

## Preset stickers

`GET /api/v1/sites/{site_id}/posts/{post_slug}/stickers`

Returns the deployment's preset stickers as
`[{ "url", "proxy_url", "alt" }]` (`url` is the `mxc://` reference used when
posting, `proxy_url` is the signed preview URL). Guests send a sticker by
posting a comment with `media.kind = "sticker"` referencing one of these
`url` values; the API rejects stickers outside the preset list.

# Operator API

Enabled by setting `security.operator_token`. All operator routes require
`Authorization: Bearer <token>`.

Operator routes are rate limited (60 requests/minute per client key).

## List sites

`QUERY /api/v1/operator/sites`

Optional JSON body (an empty body means default pagination):

```json
{ "page": 1, "per_page": 20, "site_id": "my-blog" }
```

Returns every database-tracked site merged with the operator-declared
`[sites]` overlay. Each origin carries a `source` of `"verified"` or
`"config"`. Pagination and filtering use `page`, `per_page` (1-100, default
20) and optional `site_id` from the body; the response shape is
`{ "data": [...], "meta": { "total", "page", "per_page", "total_pages" } }`.

## Revoke a verified origin

`POST /api/v1/operator/sites/{site_id}/origins/revoke`

Body: `{ "origin": "https://blog.example.com" }`. Origins declared in the
config file cannot be revoked here — edit the config instead.

## Rotate / revoke the HMAC secret

`POST /api/v1/operator/sites/{site_id}/secret/rotate` — returns the new secret
exactly once.

`DELETE /api/v1/operator/sites/{site_id}/secret` — removes the secret and falls
back to origin auth.

Both refuse to touch sites whose secret is declared in the config file.

## Export an adoption snippet

`GET /api/v1/operator/sites/{site_id}/config-snippet`

Returns `{ "site_id": "...", "toml": "..." }`; `toml` is the block to paste
into `[sites]` when the operator wants to move a database-tracked site into
declarative config.

## Rotate the claim token

`POST /api/v1/operator/sites/{site_id}/claim-token/rotate`

Returns a new `claim_token` exactly once and invalidates the previous token.
Use this when a claim token may have leaked.

## Retire a site

`DELETE /api/v1/operator/sites/{site_id}`

Operator mirror of the claim-token retire endpoint: marks the site
`retiring` immediately (writes get `410 code=site-retired`) and lets the
background pass retirement the Matrix Space/rooms and clear local
projections. See [Sites](sites.md#retire-a-site) for the full flow. Sites
declared in `[sites]` cannot be retired this way.

## List quarantined rooms

`QUERY /api/v1/operator/rooms/quarantined`

Optional JSON body with the same `page` / `per_page` / `site_id` fields as
[List sites](#list-sites).

Returns rooms whose adoption failed governance checks and are currently
quarantined, with the room id, site/page, quarantine reason, when the room
was first quarantined, how many consecutive adoption attempts failed, and
when the next automatic retry is scheduled (`null` means manual attention is
required). Quarantined rooms are retried on a 1h/6h/24h schedule; after the
fourth consecutive failure they require `reinstate`. A successful
re-registration clears the quarantine automatically. The same
pagination/filter fields and `{ "data", "meta" }` shape apply.

## Reinstate a room

`DELETE /api/v1/operator/rooms/quarantined/{room_id}`

Clears a room's quarantine and makes it the canonical room again (any other
active room for the same page is superseded). The operation is idempotent:
reinstating an already-active room also returns `204`; an unknown room
returns `404`.

## Upgrade a comment room

`POST /api/v1/operator/rooms/{room_id}/upgrade`

Body: `{"new_version": "12"}`. Upgrades a registered active comment room via
the homeserver's native `/upgrade` and converges the replacement: metadata is
repaired, the room is re-linked into its site Space (the old child's `via` is
cleared best-effort), site roles are re-invited, and the new room becomes the
registry's active room (the old one is superseded and cleaned up). The
operation is idempotent: an existing `m.room.tombstone` is reused. Spaces,
unknown rooms, non-active rooms, invalid versions and versions that are not
newer than the room's current version are rejected with a `4xx` problem
response. This endpoint is the operator mirror of the
site-level `POST /api/v1/sites/{site_id}/pages/{page_slug}/upgrade`
(claim token); both execute through the AS bot.

## Retire a comment room

`DELETE /api/v1/operator/rooms/{room_id}`

Marks the registered active room `Retired` immediately (new writes stop),
then the background reconciler renames the Matrix room `[retired]`, removes
its alias, leaves it as the AppService sender and every site virtual user,
and clears the local projections. This is the operator mirror of
`DELETE /api/v1/sites/{site_id}/pages/{page_slug}` (claim token); both go
through the same management use case. Unknown or already-retired rooms
return `404`.

## Governance fallback

The operator can act on a site's behalf for site-level roles:

- `POST /api/v1/operator/sites/{site_id}/admins` /
  `DELETE /api/v1/operator/sites/{site_id}/admins?user_id=...`
- `POST /api/v1/operator/sites/{site_id}/managers` /
  `DELETE /api/v1/operator/sites/{site_id}/managers?user_id=...`
- `POST /api/v1/operator/sites/{site_id}/ownership/transfer` — operator
  mirror of the claim-token ownership transfer.

These use the same handlers and response shapes as the claim-token
[Governance](governance.md) endpoints, including the pending-claim
verification step — the operator registers the claim, but the target MXID
must still DM the bot to activate it.

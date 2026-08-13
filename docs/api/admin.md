# Admin API

Enabled by setting `security.admin_token`. All admin routes require
`Authorization: Bearer <token>`.

Admin routes are rate limited (60 requests/minute per client key).

## List sites

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

## Revoke a verified origin

`POST /api/v1/admin/sites/{site_id}/origins/revoke`

Body: `{ "origin": "https://blog.example.com" }`. Origins declared in the
config file cannot be revoked here — edit the config instead.

## Rotate / revoke the HMAC secret

`POST /api/v1/admin/sites/{site_id}/secret/rotate` — returns the new secret
exactly once.

`DELETE /api/v1/admin/sites/{site_id}/secret` — removes the secret and falls
back to origin auth.

Both refuse to touch sites whose secret is declared in the config file.

## Export an adoption snippet

`GET /api/v1/admin/sites/{site_id}/config-snippet`

Returns `{ "site_id": "...", "toml": "..." }`; `toml` is the block to paste
into `[sites]` when the operator wants to move a database-tracked site into
declarative config.

## Rotate the claim token

`POST /api/v1/admin/sites/{site_id}/claim-token/rotate`

Returns a new `claim_token` exactly once and invalidates the previous token.
Use this when a claim token may have leaked.

## List quarantined rooms

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

## Reinstate a room

`DELETE /api/v1/admin/rooms/quarantined/{room_id}`

Clears a room's quarantine and makes it the canonical room again (any other
active room for the same post is superseded). The operation is idempotent:
reinstating an already-active room also returns `204`; an unknown room
returns `404`.

## Governance fallback

The operator can act on a site's behalf for site-level roles:

- `POST /api/v1/admin/sites/{site_id}/owners` /
  `DELETE /api/v1/admin/sites/{site_id}/owners?user_id=...`
- `POST /api/v1/admin/sites/{site_id}/co-managers` /
  `DELETE /api/v1/admin/sites/{site_id}/co-managers?user_id=...`

These use the same handlers and response shapes as the claim-token
[Governance](governance.md) endpoints, including the pending-claim
verification step — the operator registers the claim, but the target MXID
must still DM the bot to activate it.

# Governance

Site-owner operations authenticate with the claim token returned at site
registration (`X-Cumments-Claim-Token`). Operator fallbacks with the same
handlers live under `/api/v1/operator/sites/{site_id}/admins` and
`/api/v1/operator/sites/{site_id}/managers` (operator token) — see
[Operator API](operator.md#governance-fallback). Room upgrades follow the
same pattern: the site-level endpoint below, plus an operator mirror in the
Operator API.

Every role registration starts as a **pending claim**: the POST response
returns a one-time `verify_token`, and the target Matrix account must send
`cumments-claim:<token>` in a 1:1 DM with the AppService bot (a room whose
only two members are the bot and the sender) before the role is written to
Matrix power levels. The full role model and verification flow are documented
in [Site governance](../site-governance.md).

## Site admins

`POST /api/v1/sites/{site_id}/admins` /
`DELETE /api/v1/sites/{site_id}/admins?user_id=%40alice%3Aexample.com`

POST body: `{ "user_id": "@alice:example.com" }`. DELETE carries the target
in the `user_id` query parameter (DELETE bodies are avoided per RFC 9110).
Appoints or removes a site admin (level 100 in the Space and every comment
room). POST returns
`{ "pending": true, "user_id", "level", "verify_token", "expires_at" }`;
DELETE returns `{ "revoked": true, "user_id", "level" }` and cancels a
pending claim or removes an applied role; when the last site admin is
removed the response also carries a `warnings` array. Appointing the first
admin is the one-time bootstrap step.

## Site managers

`POST /api/v1/sites/{site_id}/managers` /
`DELETE /api/v1/sites/{site_id}/managers?user_id=...`

POST body: `{ "user_id": "..." }`; DELETE takes `user_id` as a query
parameter. Managers hold 75 in the Space and are replicated into every
comment room by the governance sync pass. POST returns the pending claim
shape; DELETE returns the revoked shape.

## Room moderators

`POST /api/v1/sites/{site_id}/pages/{page_slug}/moderators` /
`DELETE /api/v1/sites/{site_id}/pages/{page_slug}/moderators?user_id=...`

POST body: `{ "user_id": "..." }`; DELETE takes `user_id` as a query
parameter. Appoints or removes a moderator (level 50) in the room registered
for that page only. POST returns the pending claim shape; DELETE returns the
revoked shape.

## Read the projected rosters

`GET /api/v1/sites/{site_id}/roles` → `{ "admins": [...], "managers": [...] }`

`GET /api/v1/sites/{site_id}/pages/{page_slug}/roles` →
`{ "site_id", "page_slug", "room_id", "admins": [...], "managers": [...], "moderators": [...] }`

`GET /api/v1/sites/{site_id}/pages/{page_slug}/moderators` →
`{ "room_id": "...", "moderators": [...] }`

## Ownership transfer

`POST /api/v1/sites/{site_id}/ownership/transfer`

Body: `{ "user_id": "@new-owner:example.com" }`. Starts a two-phase transfer:
the target receives a pending admin claim and the site records a pending
transfer. Once the target sends `cumments-claim:<token>` to the bot, Cumments
resets the admin roster to the target, rotates the claim token and delivers
the new token in the bot DM. The operator mirror is
`POST /api/v1/operator/sites/{site_id}/ownership/transfer`.

## Rotate the claim token

`POST /api/v1/sites/{site_id}/claim-token-rotations`

Returns a fresh claim token and invalidates the old one. The owner can use
this immediately after a transfer to remove the delivered token from DM
history. The operator mirror is
`POST /api/v1/operator/sites/{site_id}/claim-token-rotations`.

## Upgrade a comment room

`POST /api/v1/sites/{site_id}/pages/{page_slug}/upgrades`

Body: `{"new_version": "12"}`. Upgrades the site's active comment room for
this page through the homeserver's native `/upgrade` and converges the
replacement: metadata is repaired, the room is re-linked into the site
Space (the old child's `via` is cleared best-effort), site roles are
re-invited, and the new room becomes the registry's active room (the old one
is superseded and cleaned up). The operation is idempotent: an existing
`m.room.tombstone` is reused. The upgrade itself is executed by the AS bot,
so the bot remains the replacement room's creator. Pre-v12 rooms are
upgradable when the bot holds tombstone power (new Cumments rooms grant it
150); the target version must be newer than the room's current version. The
operator mirror for raw room IDs is
`POST /api/v1/operator/rooms/{room_id}/upgrades`.

Reads come from the projected read model and are eventually consistent with
Matrix power levels.

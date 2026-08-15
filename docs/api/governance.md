# Governance

Site-owner operations authenticate with the claim token returned at site
registration (`X-Cumments-Claim-Token`). Operator fallbacks with the same
handlers live under `/api/v1/operator/sites/{site_id}/owners` and
`/api/v1/operator/sites/{site_id}/co-managers` (operator token) — see
[Operator API](operator.md#governance-fallback).

Every role registration starts as a **pending claim**: the POST response
returns a one-time `verify_token`, and the target Matrix account must send
`cumments-claim:<token>` in a 1:1 DM with the AppService bot (a room whose
only two members are the bot and the sender) before the role is written to
Matrix power levels. The full role model and verification flow are documented
in [Site governance](../site-governance.md).

## Site owners

`POST /api/v1/sites/{site_id}/owners` /
`DELETE /api/v1/sites/{site_id}/owners?user_id=%40alice%3Aexample.com`

POST body: `{ "user_id": "@alice:example.com" }`. DELETE carries the target
in the `user_id` query parameter (DELETE bodies are avoided per RFC 9110).
Adds or removes an owner (level 100 in the Space and every comment room).
POST returns
`{ "pending": true, "user_id", "level", "verify_token", "expires_at" }`;
DELETE returns `{ "revoked": true, "user_id", "level" }` and cancels a
pending claim or removes an applied role; when the last site owner is
removed the response also carries a `warnings` array. Registering the owner
is the one-time bootstrap step.

## Site co-managers

`POST /api/v1/sites/{site_id}/co-managers` /
`DELETE /api/v1/sites/{site_id}/co-managers?user_id=...`

POST body: `{ "user_id": "..." }`; DELETE takes `user_id` as a query
parameter. Co-managers hold 75 in the Space and are replicated into every
comment room by the moderation sync pass. POST returns the pending claim
shape; DELETE returns the revoked shape.

## Room moderators

`POST /api/v1/sites/{site_id}/posts/{post_slug}/moderators` /
`DELETE /api/v1/sites/{site_id}/posts/{post_slug}/moderators?user_id=...`

POST body: `{ "user_id": "..." }`; DELETE takes `user_id` as a query
parameter. Appoints or removes a moderator (level 50) in the room registered
for that post only. POST returns the pending claim shape; DELETE returns the
revoked shape.

## Read the projected rosters

`GET /api/v1/sites/{site_id}/roles` → `{ "owners": [...], "co_managers": [...] }`

`GET /api/v1/sites/{site_id}/posts/{post_slug}/moderators` →
`{ "room_id": "...", "moderators": [...] }`

Reads come from the projected read model and are eventually consistent with
Matrix power levels.

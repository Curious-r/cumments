# Site governance

Site governance lives in Matrix itself. A site is a Matrix Space; the
Space's `m.room.power_levels` and each comment room's own
`m.room.power_levels` are the **source of truth** for who may manage and
moderate it. Cumments projects those state events into disposable read
tables for API visibility; it never enforces a separate local roster.

## Roles and levels

One level ladder runs through both the Space and comment rooms:

| Role | Space | Every comment room | Meaning |
|---|---|---|---|
| Site owner | 100 | 100 | Manages the Space and every room; may edit power levels |
| Co-manager | 75 | 75 | Site-level deputy, added automatically to every room the AppService creates |
| Room moderator | — | 50 | Appointed per room, in that room only |

`ban`, `kick`, `redact` and the `state_default` thresholds stay at Matrix's
default 50, so co-managers (75) and moderators (50) have the same practical
moderation powers inside a room. The 75 level exists so site-managed roles can
be told apart from per-room moderators: the moderation sync pass reconciles
entries at **≥ 75** from the Space into every room and never touches 50.

The `m.room.power_levels` event itself is locked to 100 in both the Space and
every comment room (`events: {"m.room.power_levels": 100}`), so only owners
and the room creator (the AppService sender) can change governance. The
AppService sender is the room creator: it is the platform's backstop and is
not represented as a role.

## Day-to-day workflow

1. The site owner registers their own Matrix account through the API (or
   asks the platform operator to do it). The API returns a one-time
   verification token because a Matrix ID cannot be provisioned ahead of
   time and the ID must be proven to belong to the registrant.
2. The target Matrix account sends `cumments-claim:<token>` as a direct
   message to the AppService bot. Once the homeserver pushes that DM,
   Cumments activates the claim and writes the role into Matrix power levels.
3. Everything after that happens in a Matrix client: edit the Space's power
   levels to add/remove co-managers, and edit a comment room's power levels
   to appoint its moderators.
4. The homeserver pushes those state events to Cumments, which projects them
   into the read model. A background sync pass replicates the Space's ≥ 75
   roster into existing rooms (adding new co-managers, removing revoked
   ones), leaving per-room moderators untouched.
5. New comment rooms are seeded from the Space's roster at creation time:
   owner + co-managers, with per-room moderators starting empty.

The API offers the same operations for scripted/automated setups; registration
stores a pending claim, verification happens through the DM token, and the
normal projection brings the applied role back into the read model.

## Token-DM verification

Every role registration — including the first owner — starts as a **pending
claim** with a 24-hour expiry. The claim does not affect Matrix until the
target MXID proves ownership by DMing the exact text
`cumments-claim:<token>` to the AppService bot as a plain `m.text` message.
The projector matches the message against the pending claims for that sender
in constant time, marks the claim activated, and the reconciler then writes
the role to power levels. Re-registering the same role rotates the token;
deleting a pending role revokes the claim without touching Matrix.

## API

Site-owner operations authenticate with the claim token returned at site
registration (`X-Cumments-Claim-Token`). Operator fallbacks live under
`/api/v1/admin/sites/{site_id}/...` and use the admin token.

| Endpoint | Method | Body | Effect |
|---|---|---|---|
| `/api/v1/sites/{site_id}/owners` | POST / DELETE | `{ "user_id": "@..." }` | Register/revoke a site owner (POST returns a pending claim + token) |
| `/api/v1/sites/{site_id}/co-managers` | POST / DELETE | `{ "user_id": "@..." }` | Register/revoke a co-manager (POST returns a pending claim + token) |
| `/api/v1/sites/{site_id}/posts/{post_slug}/moderators` | POST / DELETE | `{ "user_id": "@..." }` | Register/revoke a room moderator (POST returns a pending claim + token) |
| `/api/v1/sites/{site_id}/roles` | GET | — | Projected owners and co-managers |
| `/api/v1/sites/{site_id}/posts/{post_slug}/moderators` | GET | — | Projected room moderators |

POST responses are
`{ "pending": true, "user_id", "level", "verify_token", "expires_at" }`;
DELETE responses are `{ "revoked": true, "user_id", "level" }` and are
idempotent. Reads come from the projection and are therefore eventually
consistent with Matrix.

## Platform ownership and recovery

The Cumments operator never needs a personal Matrix account: the AppService
sender is the creator of every Space and room, and the admin API / CLI can
drive it. To hand a site over, the operator rotates the claim token
(`POST /api/v1/admin/sites/{site_id}/claim-token/rotate`), gives it to the new
owner, and registers the new owner's Matrix ID via the admin mirror of the
owners endpoint.

The old `matrix.moderation.admin_id` configuration was removed. Rooms created
before this change may still carry that account's 100-level entry; registering
the site owner rewrites the site-managed entries and drops stale ones.

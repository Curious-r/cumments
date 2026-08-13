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

1. The site owner registers their own Matrix account once through the API
   (or asks the platform operator to do it). This is the only required API
   step; it exists because a Matrix ID cannot be provisioned ahead of time.
2. Everything else happens in a Matrix client: edit the Space's power levels
   to add/remove co-managers, and edit a comment room's power levels to
   appoint its moderators.
3. The homeserver pushes those state events to Cumments, which projects them
   into the read model. A background sync pass replicates the Space's ≥ 75
   roster into existing rooms (adding new co-managers, removing revoked
   ones), leaving per-room moderators untouched.
4. New comment rooms are seeded from the Space's roster at creation time:
   owner + co-managers, with per-room moderators starting empty.

The API offers the same operations for scripted/automated setups; it always
writes Matrix first (as the AppService sender) and lets the normal projection
bring the change back, so both paths converge on the same Matrix state.

## API

Site-owner operations authenticate with the claim token returned at site
registration (`X-Cumments-Claim-Token`). Operator fallbacks live under
`/api/v1/admin/sites/{site_id}/...` and use the admin token.

| Endpoint | Method | Body | Effect |
|---|---|---|---|
| `/api/v1/sites/{site_id}/owners` | POST / DELETE | `{ "user_id": "@..." }` | Add or remove a site owner |
| `/api/v1/sites/{site_id}/co-managers` | POST / DELETE | `{ "user_id": "@..." }` | Add or remove a co-manager |
| `/api/v1/sites/{site_id}/posts/{post_slug}/moderators` | POST / DELETE | `{ "user_id": "@..." }` | Add or remove a room moderator |
| `/api/v1/sites/{site_id}/roles` | GET | — | Projected owners and co-managers |
| `/api/v1/sites/{site_id}/posts/{post_slug}/moderators` | GET | — | Projected room moderators |

Write responses return the updated roster. Reads come from the projection and
are therefore eventually consistent with Matrix.

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

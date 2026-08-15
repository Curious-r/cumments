# Site governance

Site governance lives in Matrix itself. A site is a Matrix Space; the
Space's `m.room.power_levels` and each comment room's own
`m.room.power_levels` are the **source of truth** for who may manage and
moderate it. Cumments projects those state events into disposable read
tables for API visibility; it never enforces a separate local roster.

## Why this design

Governance must not conflate the platform operator with the owner of every
site: a multi-site, multi-owner deployment would otherwise leave each site
owner with a foreign high-privilege account permanently resident in their
Space. Cumments keeps the two apart:

- the AppService sender is the room creator and therefore the platform's
  backstop — it is never represented as a role;
- each site has its own **owner**, **co-manager** and per-room
  **moderator** roles, encoded purely in Matrix power levels;
- day-to-day management happens in a Matrix client; the API and Operator API
  are just other writers to the same Matrix state;
- the backend projects power levels into `site_roles` / `room_roles` and
  reconciles them, but never enforces a local copy.

## Roles and levels

One level ladder runs through both the Space and comment rooms:

| Role | Space | Every comment room | Meaning |
|---|---|---|---|
| Site owner | 100 | 100 | Manages the Space and every room; may edit power levels |
| Co-manager | 75 | 75 | Site-level deputy, added automatically to every room the AppService creates |
| Room moderator | — | 50 | Appointed per room, in that room only |

`ban`, `kick`, `redact` and the `state_default` thresholds stay at Matrix's
default 50, so co-managers (75) and moderators (50) have the same practical
moderation powers inside a room. The 75 level exists so site-managed roles
can be told apart from per-room moderators, which is what the reconciliation
pass keys on. If co-managers ever need more power than moderators, the
thresholds can move into the 50–75 gap without touching the ladder.

The `m.room.power_levels` event itself is locked to 100 in both the Space and
every comment room (`events: {"m.room.power_levels": 100}`), so only owners
and the room creator (the AppService sender) can change governance. The
AppService sender is the room creator: it is the platform's backstop and is
not represented as a role.

### The reconciliation boundary: ≥ 75, never 50

The Space is where site-level roles are managed, but every comment room has
its own copy of the power levels. A background pass keeps each room aligned
with the Space:

- entries at **≥ 75** (owner 100 + co-managers 75) are replicated from the
  Space into every room — new co-managers are added, revoked ones removed;
- entries at **50** are per-room moderators and are **never touched** by the
  pass; the site owner manages them room by room.

This makes 75 the "co-manager-only" slot: anyone holding 75 in a room must
also be a Space co-manager, otherwise the pass removes them from that room.
To let someone help in just one room, appoint them as a 50 moderator.

Example: the Space has `{owner: 100, alice: 75}` and room A has
`{owner: 100, alice: 75, bob: 50}` (bob moderates only room A):

| Change in the Space | Effect on room A | bob |
|---|---|---|
| `carol` becomes a co-manager | `carol: 75` is added to A | untouched |
| `alice`'s co-manager role is revoked | `alice: 75` is removed from A | untouched |
| bob's moderator role changes in A | no participation | changed by the owner directly |

The pass is triggered by the 60-second reconcile cycle, by an API write, and
by Space power-level pushes; it is idempotent, retried on failure, and
serialized per site. New rooms are seeded from the Space roster at creation
time (owner + co-managers, moderators start empty).

## Day-to-day workflow

1. The site owner registers their own Matrix account through the API (or
   asks the platform operator to do it). The API returns a one-time
   verification token because a Matrix ID cannot be provisioned ahead of
   time and the ID must be proven to belong to the registrant.
2. The target Matrix account sends `cumments-claim:<token>` as a direct
   message to the AppService bot in a 1:1 DM (the only two members are the
   bot and the sender). Once the homeserver pushes that DM,
   Cumments activates the claim and writes the role into Matrix power levels.
3. Everything after that happens in a Matrix client: edit the Space's power
   levels to add/remove co-managers, and edit a comment room's power levels
   to appoint its moderators.
4. The homeserver pushes those state events to Cumments, which projects them
   into the read model. The background pass keeps room-level site roles
   aligned with the Space.
5. New comment rooms are seeded from the Space's roster at creation time.

The API offers the same operations for scripted/automated setups; registration
stores a pending claim, verification happens through the DM token, and the
normal projection brings the applied role back into the read model.

## Token-DM verification

Every role registration — including the first owner — starts as a **pending
claim** with a 24-hour expiry. The claim does not affect Matrix until the
target MXID proves ownership by sending the exact text
`cumments-claim:<token>` to the AppService bot in a 1:1 DM (the only two
members are the bot and the sender).

### Claim lifecycle

```text
pending → activated → applied
   │          │
   └── revoked ┘
```

- `POST` creates (or, for an existing claim on the same role, rotates the
  token of) a `pending` claim and returns the one-time `verify_token`.
- The projector matches the DM, checks `sender == target MXID`, and marks
  the claim `activated`, then wakes the reconciler.
- The reconciler writes the role to power levels and marks it `applied`; the
  homeserver pushes the power-levels event back, and normal projection
  records the role in `site_roles` / `room_roles`.
- `DELETE` revokes a `pending` or `activated` claim without touching Matrix;
  a role that is already applied is removed from power levels directly.

The claim table is process state, not the source of truth: the applied role
is authoritative in Matrix. After a database reset, pending claims are gone
and must be re-registered (acceptable for a short-lived one-time token), while
applied roles rebuild from power levels via `backfill`.

### Matching rules

Only a plain `m.text` message with normal style and no relation activates a
claim: `m.emote` / `m.notice` are rejected, `formatted_body` is ignored (the
body is authoritative), and the token is compared in constant time against
the SHA-256 hash stored in the claim row. Non-matching DMs are silently
ignored. The bot never replies, so there is no forged callback surface.

### Security properties

- The token is 32 random bytes, hex-encoded, one-time and short-lived (24 h).
- `sender == target MXID` is guaranteed by the homeserver; Cumments cannot
  be tricked into activating a claim for another account.
- Claim creation stays behind the site's claim token (or operator token) and the
  governance rate limiter, and the number of pending claims is bounded.

## API

Site-owner operations authenticate with the claim token returned at site
registration (`X-Cumments-Claim-Token`). Operator fallbacks live under
`/api/v1/operator/sites/{site_id}/...` and use the operator token.

| Endpoint | Method | Payload | Effect |
|---|---|---|---|
| `/api/v1/sites/{site_id}/owners` | POST / DELETE | POST body or DELETE `?user_id=` | Register/revoke a site owner (POST returns a pending claim + token) |
| `/api/v1/sites/{site_id}/co-managers` | POST / DELETE | POST body or DELETE `?user_id=` | Register/revoke a co-manager (POST returns a pending claim + token) |
| `/api/v1/sites/{site_id}/posts/{post_slug}/moderators` | POST / DELETE | POST body or DELETE `?user_id=` | Register/revoke a room moderator (POST returns a pending claim + token) |
| `/api/v1/sites/{site_id}/roles` | GET | — | Projected owners and co-managers |
| `/api/v1/sites/{site_id}/posts/{post_slug}/moderators` | GET | — | Projected room moderators |

POST responses are
`{ "pending": true, "user_id", "level", "verify_token", "expires_at" }`;
DELETE responses are `{ "revoked": true, "user_id", "level" }` and are
idempotent. Reads come from the projection and are therefore eventually
consistent with Matrix.

The CLI mirrors the owner and co-manager operations locally
(`cumments sites add-owner` / `remove-owner`, `add-co-manager` /
`remove-co-manager`): `add-*` stores a pending claim and prints the
`verify_token`, while `remove-*` revokes a pending claim. The CLI never
writes power levels, so an already-applied role must be removed from a
Matrix client or the Operator API (see [CLI](cli.md)).

## Platform ownership and recovery

The Cumments operator never needs a personal Matrix account: the AppService
sender is the creator of every Space and room, and the Operator API / CLI can
drive it. To hand a site over, the operator rotates the claim token
(`POST /api/v1/operator/sites/{site_id}/claim-token/rotate`), gives it to the new
owner, and registers the new owner's Matrix ID via the operator mirror of the
owners endpoint.

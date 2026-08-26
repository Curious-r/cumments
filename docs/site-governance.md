# Site governance

Site governance lives in Matrix itself. A site is a Matrix Space; the
Space's `m.room.power_levels` and each comment room's own
`m.room.power_levels` are the **source of truth** for who may manage and
moderate it. Cumments projects those state events into disposable read
tables for API visibility; it never enforces a separate local roster.

## Why this design

Governance must not conflate the platform operator with the owner of every
site: a multi-site deployment would otherwise leave each site owner with a
foreign high-privilege account permanently resident in their Space. Cumments
keeps the two apart:

- the AppService sender is the room creator and therefore the platform's
  backstop — it is never represented as a role;
- **ownership** is a claim-token capability held by the site owner;
- each site has **site admins** (100), **managers** (75) and per-room
  **moderators** (50), encoded purely in Matrix power levels;
- day-to-day management happens in a Matrix client or through the bot; the
  API and Operator API are just other writers to the same Matrix state;
- the backend projects power levels into `site_roles` / `room_roles` and
  reconciles them, but never enforces a local copy.

## Roles and levels

| Role | Space | Every comment room | Meaning |
|---|---|---|---|
| Site Admin | 100 | 100 | Full site-level operational powers; may manage managers, moderators, stickers, secrets, retirement and upgrades |
| Manager | 75 | 75 | Site-level deputy, replicated into every comment room; may appoint/revoke room moderators |
| Room moderator | — | 50 | Appointed per room, in that room only |

The site **owner** is not a Matrix role: the owner is whoever holds the
site's claim token. Owners appoint and remove site admins and transfer
ownership through the claim-token API. A site may have zero or more site
admins; the owner's own Matrix account is usually the first one.

`ban`, `kick`, `redact` and the `state_default` thresholds stay at Matrix's
default 50, so managers (75) and moderators (50) have the same practical
moderation powers inside a room. The 75 level exists so site-managed roles
can be told apart from per-room moderators, which is what the reconciliation
pass keys on.

The `m.room.power_levels` event itself is locked to **100 in the Space** and
**75 in comment rooms** (`events: {"m.room.power_levels": 75}` in rooms),
and `m.room.tombstone` is locked to 150. Managers can therefore appoint room
moderators exactly like a Matrix client, while only site admins and the room
creator (the AppService sender) can change the Space. The AppService sender
is the room creator: it is the platform's backstop and is not represented as
a role.

### The reconciliation boundary: ≥ 75, never 50

The Space is where site-level roles are managed, but every comment room has
its own copy of the power levels. A background pass keeps each room aligned
with the Space:

- entries at **≥ 75** (admins 100 + managers 75) are replicated from the
  Space into every room — new managers are added, revoked ones removed;
- entries at **50** are per-room moderators and are **never touched** by the
  pass; site admins and managers manage them room by room.

This makes 75 the "manager-only" slot: anyone holding 75 in a room must also
be a Space manager, otherwise the pass removes them from that room. To let
someone help in just one room, appoint them as a 50 moderator.

Example: the Space has `{admin: 100, alice: 75}` and room A has
`{admin: 100, alice: 75, bob: 50}` (bob moderates only room A):

| Change in the Space | Effect on room A | bob |
|---|---|---|
| `carol` becomes a manager | `carol: 75` is added to A | untouched |
| `alice`'s manager role is revoked | `alice: 75` is removed from A | untouched |
| bob's moderator role changes in A | no participation | changed by an admin/manager |

The pass is triggered by the 60-second reconcile cycle, by an API write, and
by Space power-level pushes; it is idempotent, retried on failure, and
serialized per site. New rooms are seeded from the Space roster at creation
time (admins + managers, moderators start empty).

## Day-to-day workflow

1. **Self-service**: the site owner sends `!cumments site register <id>`
   in a DM with the bot. The bot registers the site, creates the Space and
   writes the sender as the first **site admin** immediately. Alternatively,
   register through the API and appoint any Matrix account as the first
   admin (the API returns a one-time verification token because the
   registrant and target are not necessarily the same account).
2. For API/operator-appointed roles, the target Matrix account sends
   `cumments-claim:<token>` as a direct
   message to the AppService bot in a 1:1 DM (the only two members are the
   bot and the sender). Once the homeserver pushes that DM, Cumments
   activates the claim and writes the role into Matrix power levels.
3. Site admins manage the Space in a Matrix client or through the bot: they
   add/remove managers, retire rooms, issue secrets and appoint room
   moderators.
4. Managers appoint and revoke room moderators through their Matrix client
   or the bot; the room's power-levels threshold (75) lets them edit the
   room exactly like a Matrix client would.
5. The homeserver pushes state events to Cumments, which projects them into
   the read model. The background pass keeps room-level site roles aligned
   with the Space.

## Token-DM verification

Every role registration through the API/operator path — including the first
site admin — starts as a **pending claim** with a 24-hour expiry. The claim
does not affect Matrix until the target MXID proves ownership by sending the
exact text `cumments-claim:<token>` to the AppService bot in a 1:1 DM (the
only two members are the bot and the sender). The bot's self-service
`site register` path skips this second step because the homeserver has
already authenticated the sender in the DM; it records an applied claim
instead.

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
- Claim creation stays behind the site's claim token (or operator token) and
  the governance rate limiter, and the number of pending claims is bounded.

## API

Site-owner (claim-token) operations authenticate with the token returned at
site registration (`X-Cumments-Claim-Token`). Operator fallbacks live under
`/api/v1/operator/sites/{site_id}/...` and use the operator token.

| Endpoint | Method | Payload | Effect |
|---|---|---|---|
| `/api/v1/sites/{site_id}/admin-claims` | POST | `{ "user_id": ... }` | Create a site admin claim |
| `/api/v1/sites/{site_id}/admins/{user_id}` | DELETE | — | Revoke a pending/applied site admin |
| `/api/v1/sites/{site_id}/manager-claims` | POST | `{ "user_id": ... }` | Create a manager claim |
| `/api/v1/sites/{site_id}/managers/{user_id}` | DELETE | — | Revoke a pending/applied manager |
| `/api/v1/sites/{site_id}/pages/{page_slug}/moderators` | POST / DELETE | POST body or DELETE `?user_id=` | Appoint/revoke a room moderator (POST returns a pending claim + token) |
| `/api/v1/sites/{site_id}/roles` | GET | — | Projected admins and managers |
| `/api/v1/sites/{site_id}/pages/{page_slug}/roles` | GET | — | Projected admins, managers and moderators |
| `/api/v1/sites/{site_id}/ownership-transfers` | POST | `{ "user_id": ... }` | Start a two-phase ownership transfer |
| `/api/v1/sites/{site_id}/claim-token-rotations` | POST | — | Owner rotates their own claim token |

POST responses are
`{ "pending": true, "user_id", "level", "verify_token", "expires_at" }`;
DELETE responses are `{ "revoked": true, "user_id", "level" }` and are
idempotent. Reads come from the projection and are therefore eventually
consistent with Matrix.

The CLI mirrors the admin and manager operations locally
(`cumments sites add-admin` / `remove-admin`, `add-manager` /
`remove-manager`) and can start a transfer with `cumments sites transfer-owner`:
`add-*` stores a pending claim and prints the `verify_token`, while
`remove-*` revokes a pending claim. The CLI never writes power levels, so an
already-applied role must be removed from a Matrix client or the Operator
API (see [CLI](cli.md)).

## Ownership transfer and recovery

The owner (claim-token holder) can start a two-phase transfer with
`POST /api/v1/sites/{site_id}/ownership-transfers`. The target verifies the
usual `cumments-claim` token; once verified, Cumments resets the site-admin
roster to the new owner's verified account, rotates the claim token and
delivers the new token in the bot DM. Old site admins are removed with the
transfer.

If the claim token is lost, the Cumments operator can rotate it through the
Operator API and appoint a fresh site admin. The AppService sender is the
creator of every Space and room, so it remains the last-resort backstop even
when a site has no admins.

# Architecture

Cumments is a decentralized comment system backend built on the **Matrix
protocol**. Matrix is the **source of truth**: every comment, edit and
deletion is an immutable Matrix event. SQLite is a disposable local read model
that can be rebuilt from Matrix history with `cumments backfill`.

How Matrix events are shaped into the typed comment model is documented in
[Data model](data-model.md).

## Design philosophy

Everything below follows from three invariants:

1. **Matrix is the only source of truth.** Comments, roles and room state
   live in Matrix events; the local SQLite is a disposable projection that
   `cumments backfill` can rebuild. Nothing local is authoritative.
2. **One write seam.** Every homeserver write initiated by Cumments goes
   through `MatrixDriver`. The API and reconciler never call the homeserver
   directly for state changes; the CLI only writes local intent rows that
   the reconciler later applies. The driver authenticates with the
   AppService `as_token` and sends either as the AppService sender (room
   creation, state events, redactions) or as a virtual user in the
   `@_cumments_.*` namespace (visitor messages, media uploads). Read paths
   (backfill, ephemeral `/sync`, media proxy fetches) stay outside this
   seam because they do not mutate Matrix state.
3. **Push closes the loop.** In AppService mode the homeserver pushes events
   back, the projector updates the read model, and the reconciler confirms
   its work. The same idempotent projection also serves `backfill`; Logging
   mode has no homeserver push and therefore no closed loop.

Together these collapse the design space into one conceptual skeleton:
durable local submission of the user's intent, background convergence on
Matrix, then projection of the result. How much machinery a feature needs
sits on the spectrum below, not in a second shape. There are also two
control loops with different timings: the write path is
`intent → act → observe → confirm`, while convergence passes such as
governance sync are `observe → diff → act`. Both rely on idempotency and
replay, and both match Matrix itself, which is an append-only event log with
full-state events.

### Background-action intensity spectrum

Not every background action is a full submission queue. The mechanisms form a
spectrum of how much machinery they need:

| Mechanism | Weight | Idempotency anchor |
|---|---|---|
| Comment submissions (post, edit, delete, location) | Heavy | `Idempotency-Key` + request fingerprint + claimable submission row (lease for crash recovery), with timeouts, dead-lettering and room quarantine |
| Role claims (token-DM) | Light state machine | Claim row plus sender/token match (`pending` → `activated` → `applied`) |
| Moderation sync | Pure convergence | Matrix power levels are full state, so read → diff → write is naturally idempotent |
| Site retirement | One-shot marker, retry until converged | `lifecycle_status` plus idempotent rename / alias removal / leave (404-tolerant) |
| Orphan media cleanup | Periodic sweep | Unreferenced-upload marker |

The deciding question for each is how strong the once-only guarantee must be:
write-side idempotency keys for work that must not duplicate, natural
idempotency for work that merely has to converge.

### Submission durability

`202 { "submission_id": ... }` means the submission row is durably recorded
in the local SQLite queue and will be retried while that record survives. It
does **not** mean the Matrix event exists yet. Matrix becomes the source of
truth only when the event is written; until then the submission is local
coordination state. If the local database is destroyed before the reconciler
writes the event, the accepted submission can be lost, and `backfill` cannot
recover it because no Matrix event was ever created. The practical contract
is best-effort convergence from a locally durable queue, not at-least-once
delivery across local-database loss. Idempotency keys protect against
duplicate submissions while the local record survives.

### Boundaries to watch

- The reconciler is a set of independent controllers, one task per pass, each
  with its own wakeup channel and resync interval. As the pass count grows,
  scheduling priorities and per-pass observability are the things to watch.
- Rate limiters and the reconcile loop assume a single instance. Distributed
  deployments would need a shared limiter store and leader election /
  partitioning — a documented platform limitation.
- Claims, retirement and orphan-media cleanup share the *concept* of a
  background action but not its shape (a row machine, an entity lifecycle and
  a table sweep, respectively). A generic action ledger is therefore deferred
  until a second genuine per-action row machine appears — sharing a concept
  does not imply sharing a table.

## System overview

```
                    ┌──────────────────┐
                    │  User Request    │
                    │  (Browser/API)   │
                    └────────┬─────────┘
                             │
                ┌────────────▼────────────┐
                │      cumments-api       │
                │ (HTTP, PoW, Ed25519)    │
                └────────────┬────────────┘
                             │ submission
                ┌────────────▼────────────┐
                │    Submission Queue     │
                │         (SQLite)        │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │       Reconciler        │
                │     (writer path)       │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │      MatrixDriver       │
                │ (AppService / Logging)  │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │  Matrix homeserver      │
                │   (source of truth)     │
                └────────────┬────────────┘
                             │ push (AppService)
                ┌────────────▼────────────┐
                │      PushReceiver       │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │     EventProcessor      │
                │ (idempotent projection) │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │  SQLite read model      │
                │ (disposable, rebuildable)│
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │ API queries / SSE       │
                └─────────────────────────┘
```

The **write path** is submission-driven: the API validates the proof-of-work and
an Ed25519 signature, enqueues a submission, and the **Reconciler** sends the
corresponding event to Matrix. The **read path** is projection-based: in
AppService mode, `PushReceiver` receives events via homeserver push and feeds
them into **EventProcessor**, which updates the SQLite read model and emits
SSE. `cumments backfill` reuses the same idempotent projection.

## Operation modes

### AppService mode (production)

Cumments registers as a Matrix Application Service. Each visitor is
represented by a deterministic virtual user:

```text
@_cumments_{site_id}_{sha256(public_key) first 16 bytes, hex}:{server_name}
```

The registration reserves exclusive `users` and `aliases` namespaces
(`@_cumments_.*` and `#_cumments_.*`); room IDs are not namespaced because
they are generated by the homeserver.

| Entity | Matrix identifier |
|---|---|
| Visitor virtual user | `@_cumments_{site_id}_{sha256(public_key) first 16 bytes, hex}:{server_name}` |
| AppService sender | `@_cumments_bot:{server_name}` (configurable via `matrix.appservice.sender_localpart`) |
| Site space alias | `#_cumments_{site_id}:{server_name}` |
| Comment room alias | `#_cumments_{site_id}_{page_slug}:{server_name}` |
| Room ID | generated by the homeserver (`!...:{server_name}`) |

Custom Matrix identifiers use the reverse-DNS namespace
`host.curious.cumments`:

- room identity lives in the `host.curious.cumments.metadata` state event;
- Cumments-specific message fields live under
  `host.curious.cumments.message`;
- the signed delete proof lives under `host.curious.cumments.redaction`;
- the Matrix profile field keeps its spec name `displayname`, while Rust,
  database and Cumments API code uses `display_name`.

The message proof block carries only identity and content fields
(`public_key`, `signature`, `challenge`, `content`, `submission_id`);
display data is profile state and never enters the proof domain.

The homeserver pushes events to `PUT /_matrix/app/v1/transactions/{txnId}`,
authenticated with `hs_token` (Bearer header, legacy query fallback).

### Site governance

Governance follows the same source-of-truth principle: a site is a Matrix
Space, and the Space's `m.room.power_levels` plus each comment room's own
`m.room.power_levels` define who manages the site and moderates its rooms.
The level ladder is 100 (site admin), 75 (manager, replicated from the Space
into every room) and 50 (per-room moderator). The power-levels event itself
is locked to 100 in the Space and 75 in comment rooms, so site admins (and
the AppService sender) govern the Space while managers can appoint room
moderators. Push projection stores the rosters in disposable `site_roles` /
`room_roles` tables, a reconciler pass keeps room-level site roles aligned
with the Space, and `backfill` replays Space state events so the rosters
rebuild after a database reset. See
[site governance](site-governance.md) for the full model.

### Bot management channel

The AppService sender doubles as a management bot: `@_cumments_bot` accepts
`!cumments` commands and role-claim tokens (`cumments-claim:...`) in private
DMs. This is the third management pathway next to the Operator API and the
CLI. All three call the same `cumments_core::management` use cases, so chat
commands never open a separate authority or a second write seam.

Security model:

- Commands only execute in a verified private channel — exactly the bot and
  the sender joined — and fail closed when membership cannot be verified.
  Elsewhere a `!cumments` message is consumed silently so it never becomes a
  comment.
- Commands are partitioned by the caller's role. Instance operators
  (`security.operator_mxids`) may list sites, rotate claim tokens, list
  quarantined rooms, reinstate rooms, upgrade comment rooms and trigger
  backfill. Site admins may register/retire their own sites, manage managers
  and moderators, issue secrets, and switch the active site; managers may
  appoint/revoke room moderators; public self-service (`site register`,
  `site use`, `site status`) needs no operator configuration.
- Prefix messages pass through an in-process global admission budget before
  the private-channel membership lookup. Once a channel is verified, each
  sender has a bounded per-MXID command budget. Throttled commands are
  consumed silently so they do not produce bot replies, audit writes, or
  AppService retries.
- Commands that pass admission are written to the command audit trail with a
  status (ok / invalid / denied / error).
- Role claims are capabilities. The bot auto-joins a DM only when the inviter
  has a pending claim, so it cannot be pulled into arbitrary rooms. Claim DMs
  must stay unencrypted because the token is plain text; an
  `m.room.encryption` event is warned about and ignored.

Backfill is also available to operators as `!cumments backfill [max_pages]`;
it queues a worker request and the bot replies in the DM when the worker
finishes.

### Room upgrades

Comment rooms can be upgraded to a new room version through the homeserver's
native `/upgrade`. The primary path is site-level (a site admin decides to
upgrade their own room) with an operator mirror as fallback: site admins use
`POST /api/v1/sites/{site_id}/pages/{page_slug}/upgrade` (claim token) or
`!cumments site <id> page <slug> upgrade <version> --confirm`; operators use
`cumments rooms upgrade ROOM_ID VERSION`,
`POST /api/v1/operator/rooms/{room_id}/upgrade`, or
`!cumments room ROOM_ID upgrade VERSION --confirm` in a private DM. The
target version must be newer than the room's current version. The driver is
idempotent: an existing `m.room.tombstone` is reused, and a failed request
re-reads the tombstone before reporting an error, so a lost response cannot
mint a second replacement room.

#### Governance attribution: site-level motivation, instance-level execution

The room belongs to a site and the site admin (level 100) is its highest
governance role, so upgrading one of the site's rooms is a **site-level
operation** with an operator mirror, exactly like retiring the site: the
admin triggers it through the claim-token API or the bot, and the instance
operator can act as a fallback. The reasons for the split are:

- In room version 12 the caller of `/upgrade` becomes the new room's creator
  with immutable infinite power. A site admin upgrading directly would
  escalate from governance level 100 to creator power and could lock the bot
  out of the replacement room. With the bot as the caller, the bot remains
  the creator and the admin keeps level 100.
- An upgrade mutates shared invariants (alias, registry single-active
  supersede, cleanup, Space re-link); they stay inside the shared management
  seam and the operator mirror exists for fallback, but the admin's own
  rooms are not an adoption-trust matter like quarantine/reinstate.
- Every upgrade mints a new room; repeated upgrades create orphaned
  replacements and role re-invites, bounded by the site admin's own scope.

Implemented entry points:

- Site admin: `POST /api/v1/sites/{site_id}/pages/{page_slug}/upgrade`
  (claim token) and `!cumments site <id> page <slug> upgrade <version>
  --confirm` (private DM, site-admin permission).
- Operator mirror: `POST /api/v1/operator/rooms/{room_id}/upgrade`
  (operator token) and `!cumments room <id> upgrade <version> --confirm`.

The bot is always the `/upgrade` caller in every path, so it stays the
replacement room's creator.

#### Convergence design

The native upgrade does not fully converge a Cumments room, so the management
use case owns these writes:

- **Metadata**: the spec says not to transfer sender-sensitive non-Matrix
  state; `host.curious.cumments.metadata` is therefore re-written during
  adoption of the replacement room.
- **Space graph**: `/upgrade` does not update references in other rooms, and
  MSC4168 (still open) is only partially implemented by homeservers —
  tuwunel copies `m.space.parent`/`m.space.child` into the new room but does
  not update the parent Space. Cumments re-links the Space child to the
  replacement and best-effort clears the old child's `via` so clients stop
  treating the tombstoned room as part of the Space.
- **Membership**: memberships are not transferred. Site roles (>= 75) from
  the Space power levels are re-invited; per-room moderators and ordinary
  users can join the public replacement room themselves.
- **Registry**: registering the replacement as active automatically
  supersedes the old room; the room-cleanup pass then retires the old room's
  AS-managed memberships.

#### Current compromises

Room upgrade is built on a stable endpoint (`/upgrade`), but several
surrounding mechanisms are still open proposals. Until they mature, the
implementation carries these documented compromises:

- **Manual Space-graph repair.** MSC4168 (open) would have homeservers copy
  and update `m.space.*` references across rooms; tuwunel only copies state
  into the new room. Cumments therefore re-links the Space child and
  best-effort clears the old child's `via` itself.
- **No Space upgrades.** Upgrading a site Space would orphan every child
  room's `m.space.parent`, lose sticker packs (image-pack state is not in
  the recommended transfer list), and leave the `sites` mapping ambiguous
  for backfill. While MSC4168/MSC4433 are open, only comment rooms are
  upgradable.
- **No image-pack migration.** MSC4433 (open) is not implemented; it is a
  non-issue for comment rooms because packs live in the site Space, and
  Space upgrades are out of scope for now.
- **Target-newer check is ours.** The spec allows upgrading to any supported
  version, including older ones; Cumments rejects targets that are not
  newer so the operation stays an actual upgrade.
- **Legacy pre-v12 rooms are not upgradable.** Room versions 1-11 give no
  implicit creator power and the auth rules forbid the bot from raising
  itself above 100; tuwunel's admin `make_room_admin` only grants the
  room's highest local level (100). New pre-v12 rooms get the bot at 150
  from creation, but rooms created before that policy are an accepted
  breaking change while there are no production instances.

These compromises are expected to shrink or disappear as the tracked
standards land; see below.

#### Tombstone threshold

Initial power levels lock `m.room.tombstone` to 150 (the room version 12
recommended value) and the governance pass normalizes existing rooms. This
blocks both 50-level moderators and 100-level site admins from sending a
tombstone in a Matrix client, so only the room creator (the AS bot, who has
infinite power in v12) can upgrade. A client-side upgrade by a site admin
would otherwise make them the replacement room's creator with immutable
infinite power, bypassing the convergence loop. In v12 rooms the bot's
creator power is implicit; in pre-v12 rooms the initial power levels give
the bot an explicit 150 entry so it can still upgrade. Legacy pre-v12 rooms
created before this policy (where the bot is only 100) have no in-product
upgrade path and are an accepted breaking change. The target version must
be newer than the room's current version: Matrix itself does not forbid
downgrades, so Cumments enforces this at the use-case level.

#### Standard tracking

These open standards shape this design; revisit it when they land:

| Standard | Status (2026-08) | Impact when merged |
|---|---|---|
| [MSC4168: update `m.space.*` on room upgrade](https://github.com/matrix-org/matrix-spec-proposals/pull/4168) | open; tuwunel implements the copy half only | homeservers update child/parent references themselves; convergence can shrink to idempotent re-check plus old-child `via` clearing |
| [MSC4433: image packs and room upgrades](https://github.com/matrix-org/matrix-spec-proposals/pull/4433) | open | comment-room upgrades unaffected (packs live in the site Space); revisit for Space upgrades |
| [Room Upgrades module](https://spec.matrix.org/v1.19/client-server-api/#room-upgrades) | stable in v1.19 | re-check the recommended transfer list on every spec upgrade; new copied types may simplify convergence |

Review triggers:

- a tracked MSC merges into the spec;
- a Matrix spec release changes the Room Upgrades server-behavior list;
- the deployed homeserver (tuwunel) implements one of the tracked MSCs.

Policy: when a formal, complete standard exists, revise the implementation
to follow it instead of keeping the manual convergence, and update the
compromises above together with the code.

### Logging mode (local development)

The `LoggingMatrixDriver` logs actions instead of talking to a homeserver.
Useful for exercising the API and the submission queue without Matrix side
effects. Comments are **not** projected into the read model because there is
no homeserver pushing events back.

### Ephemeral events

Comments arrive over AppService push transactions, but typing indicators,
read receipts and presence are not part of room history. Cumments keeps a
resident `/sync` connection (AppService token + bot identity) that filters
for ephemeral/presence only, holds per-room in-memory state, and feeds
incremental `ephemeral` events — plus an initial snapshot — into the same SSE
stream as comment updates. See
[Ephemeral events](data-model.md#ephemeral-events).

## Crates

| Crate | Responsibility |
|---|---|
| `cumments-core` | Domain models, ports (traits), commands, submissions, events, governance helpers |
| `cumments-api` | HTTP API, PoW verification, validation, SSE |
| `cumments-store` | SQLite persistence (SeaORM), migrations, backup |
| `cumments-reconciler` | Background writer — reads submissions, calls MatrixDriver, waits for projection to close the loop, reconciles site roles |
| `cumments-matrix` | MatrixDriver implementations (AppService, Logging) |
| `cumments-projector` | Event reception and projection (EventProcessor, PushReceiver, claim-DM matching, ephemeral sync, backfill, chat command routing) |
| `cumments` | CLI entry point, configuration, assembly |
| `cumments-test-utils` | Test-only shared doubles (MatrixDriver fake) for workspace tests |

## Recovery

### Backfill

```bash
cumments backfill
```

`cumments backfill` reconstructs the SQLite read model from Matrix history. It
requires an AppService configuration connected to a reachable homeserver:

1. discovers Cumments rooms via `get_joined_rooms` and
   `host.curious.cumments.metadata` (restores sites, their Spaces and the
   room registry after a local DB reset);
2. paginates each comment room's and Space's history via the CS API
   `/messages`;
3. replays events in `(origin_server_ts, event_id)` order through the same
   idempotent projection used for live pushes.

`cumments backfill --max-pages N` caps how much history is fetched per room
(~100 events each). Fetched events are buffered in memory so the chronological
replay can apply edits/redactions after their targets; the cursor is saved so
a later run resumes where it stopped. `0` disables the cap (default: 500). A
hard in-memory buffer bound also applies per room: hitting it fails that room
with a clear error instead of exhausting memory, and the room is rerun with a
smaller `--max-pages`.

### Backup

```bash
cumments backup --output data/cumments.backup.db
```

Runs a WAL checkpoint and writes a consistent single-file SQLite snapshot via
`VACUUM INTO`. The destination must not already exist. Snapshots are a
convenience; `backfill` is the authoritative recovery path. Opening the source
database runs any pending migrations first, so the source may be upgraded by
the backup command.

## Known limitations

- Reply trees use Matrix rich replies (`m.in_reply_to`) with no depth limit;
  the demo UI only collapses rendering past 8 levels. Email is deliberately
  not collected.
- Distributed/global rate limiting, multi-instance/Postgres support, and
  operational monitoring are not implemented yet. In-process rate limits
  cover site registration/verification, verification confirm, comment
  writes, SSE connections, the Operator API, and Matrix bot commands.
- Matrix-native comments bypass the API's PoW by design; spam in that path is
  governed by Matrix room moderation (power levels, bans, etc.).
- `m.space.child` events only refresh rooms already known to the local
  registry; unknown rooms linked through a Space are picked up by the
  reconciler or `backfill` instead of being auto-registered.
- Comment-room upgrades are supported through the homeserver's native
  `/upgrade`: `cumments rooms upgrade <room_id> <version>`, the Operator API
  (`POST /api/v1/operator/rooms/{room_id}/upgrade`), or the bot
  (`!cumments room <room_id> upgrade <version> --confirm`). The replacement
  room is adopted (metadata repaired), re-linked into the site Space, site
  roles are re-invited, and the old room is superseded and cleaned up. Site
  Space upgrades are not supported: MSC4168 (updating `m.space.*` references
  across rooms) is still open, and a Space upgrade would require re-linking
  every child room plus copying sticker-pack state. Image-pack handling
  across room upgrades (MSC4433) is not implemented.
- State resolution is approximated with latest-wins on
  `(origin_server_ts, event_id)`. This is fine on a single homeserver but is
  not a full DAG/mainline state resolution, so forked or federated rooms may
  diverge from the homeserver's resolved state.
- Redaction of state events follows the room-version 11+ algorithm in the
  projector (protected keys are kept, other content is emptied in place);
  membership, redaction acceptance and state resolution themselves are
  delegated to the homeserver and have only been verified against a
  single-server tuwunel deployment.

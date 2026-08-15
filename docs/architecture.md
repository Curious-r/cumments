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
   `@_cumments_.*` namespace (guest messages, media uploads). Read paths
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
| Site decommission | One-shot marker, retry until converged | `lifecycle_status` plus idempotent rename / alias removal / leave (404-tolerant) |
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
- Claims, decommission and orphan-media cleanup share the *concept* of a
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
@_cumments_{site_id}_{sha256(public_key) first 8 bytes, hex}:{server_name}
```

The registration reserves exclusive `users` and `aliases` namespaces
(`@_cumments_.*` and `#_cumments_.*`); room IDs are not namespaced because
they are generated by the homeserver.

| Entity | Matrix identifier |
|---|---|
| Visitor virtual user | `@_cumments_{site_id}_{sha256(public_key) first 8 bytes, hex}:{server_name}` |
| AppService sender | `@_cumments_bot:{server_name}` (configurable via `matrix.appservice.sender_localpart`) |
| Site space alias | `#_cumments_{site_id}:{server_name}` |
| Comment room alias | `#_cumments_{site_id}_{post_slug}:{server_name}` |
| Room ID | generated by the homeserver (`!...:{server_name}`) |

Custom Matrix identifiers use the reverse-DNS namespace
`host.curious.cumments`:

- room identity lives in the `host.curious.cumments.metadata` state event;
- Cumments-specific message fields live under
  `host.curious.cumments.message`;
- the signed delete proof lives under `host.curious.cumments.redaction`;
- the Matrix profile field keeps its spec name `displayname`, while Rust,
  database and Cumments API code uses `display_name`.

The homeserver pushes events to `PUT /_matrix/app/v1/transactions/{txnId}`,
authenticated with `hs_token` (Bearer header, legacy query fallback).

### Site governance

Governance follows the same source-of-truth principle: a site is a Matrix
Space, and the Space's `m.room.power_levels` plus each comment room's own
`m.room.power_levels` define who manages the site and moderates its rooms.
The level ladder is 100 (owner), 75 (co-manager, replicated from the Space
into every room) and 50 (per-room moderator). The power-levels event itself
is locked to 100 so only owners and the AppService sender can change
governance. Push projection stores the rosters in disposable `site_roles` /
`room_roles` tables, a reconciler pass keeps room-level site roles aligned
with the Space, and `backfill` replays Space state events so the rosters
rebuild after a database reset. See
[site governance](site-governance.md) for the full model.

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
| `cumments-projector` | Event reception and projection (EventProcessor, PushReceiver, claim-DM matching, ephemeral sync, backfill) |
| `cumments` | CLI entry point, configuration, assembly |

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
  writes, SSE connections, and the Operator API.
- Matrix-native comments bypass the API's PoW by design; spam in that path is
  governed by Matrix room moderation (power levels, bans, etc.).
- `m.space.child` events only refresh rooms already known to the local
  registry; unknown rooms linked through a Space are picked up by the
  reconciler or `backfill` instead of being auto-registered.

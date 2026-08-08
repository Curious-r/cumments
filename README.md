# Cumments

[English](README.md) | [中文](README.zh-CN.md)

Cumments is a decentralized comment system backend built on the **Matrix protocol**.
Matrix is the **source of truth**: every comment, edit, and deletion is an immutable
Matrix event. SQLite is a disposable local read model that can be rebuilt from
Matrix history with `cumments backfill`.

## Highlights

- **Matrix as the event log** — comments are `m.room.message` events, edits are
  `m.replace`, and deletions are `m.redaction`.
- **Publicly verifiable ownership** — each event carries the author's Ed25519
  public key and signature (`cumments_public_key` / `cumments_signature`), so
  ownership survives a complete read-model rebuild.
- **Disposable read model** — SQLite is only a projection; `cumments backfill`
  rebuilds sites, the room registry, and comments from Matrix history.
- **AppService-first** — production mode registers as a Matrix Application
  Service, uses virtual users, and receives events via HTTP push. No bot mode.
- **PoW anti-spam** — comments require solving a signed proof-of-work challenge;
  no login or account system.
- **Real-time SSE updates** — `new_comment`, `comment_updated`, and
  `comment_deleted` events.

## Architecture

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
                             │ intent
                ┌────────────▼────────────┐
                │      Intent Queue       │
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

The write path is intent-driven: the API validates PoW and an Ed25519
signature, enqueues an intent, and the **Reconciler** sends the corresponding
event to Matrix. The read path is projection-based: in AppService mode,
`PushReceiver` receives events via homeserver push and feeds them into
**EventProcessor**, which updates the SQLite read model and emits SSE.
`cumments backfill` reuses the same idempotent projection.

## Operation Modes

### AppService Mode (production)

Cumments registers as a Matrix Application Service. Each visitor is represented
by a deterministic virtual user:

```text
@_cumments_{site_id}_{sha256(public_key) first 4 bytes, hex}:{server_name}
```

The homeserver pushes events to `PUT /_matrix/app/v1/transactions/{txnId}`,
authenticated with `hs_token`. No sync loop is required.

### Logging Mode (local development)

The `LoggingMatrixDriver` logs actions instead of talking to a homeserver.
Useful for exercising the API and the local read model without Matrix side
effects.

## Recovery

### Backfill

```bash
cumments backfill
```

`cumments backfill` reconstructs the SQLite read model from Matrix history.
It requires an AppService configuration connected to a reachable homeserver:

1. discovers Cumments rooms via `joined_rooms` and `im.cumments.metadata`
   (restores sites and the room registry after a local DB reset);
2. paginates each comment room's history via the CS API `/messages`;
3. replays events in `(origin_server_ts, event_id)` order through the same
   idempotent projection used for live pushes.

Interrupted runs resume from persisted per-room cursors.

### Backup

```bash
cumments backup --output data/cumments.backup.db
```

Runs a WAL checkpoint and writes a consistent single-file SQLite snapshot via
`VACUUM INTO`. The destination must not already exist. Snapshots are a
convenience; `backfill` is the authoritative recovery path.

## Crates

| Crate | Responsibility |
|---|---|
| `cumments-core` | Domain models, ports (traits), intents, events |
| `cumments-api` | HTTP API, PoW verification, validation, SSE |
| `cumments-store` | SQLite persistence (SeaORM), migrations, backup |
| `cumments-reconciler` | Background writer — reads intents, calls MatrixDriver, waits for projection to close the loop |
| `cumments-matrix` | MatrixDriver implementations (AppService, Logging) |
| `cumments-projector` | Event reception and projection (EventProcessor, PushReceiver, backfill) |
| `cumments` | CLI entry point, configuration, assembly |

## Configuration

Configuration is loaded in this order:

1. Environment variables (`CUMMENTS__` prefix, `__` separator)
2. `config.toml` (or `--config <path>`)
3. Defaults

Example AppService configuration:

```toml
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"
public_server_name = "your_server.tld"

[database]
url = "sqlite://data/cumments.db"

[security]
admin_token = "admin_secret"
pow_secret = "pow_secret_key"
pow_difficulty = 4

[matrix]
mode = "appservice"
homeserver_url = "http://localhost:8008"
server_name = "your_server.tld"
as_token = "${AS_TOKEN}"
hs_token = "${HS_TOKEN}"
bot_localpart = "cumments"
push_listen_port = 3001
owner_id = "@admin:your_server.tld"
```

For local development, set `mode = "logging"`; only `homeserver_url` and
`owner_id` are then required.

Configuration notes:

- `admin_token`, `cors_origins`, and `public_server_name` are currently parsed
  but not yet enforced; CORS is permissive.
- SQLite files are created automatically, but the parent directory must exist
  (the repo has a `data/` directory).
- All timestamps are stored in UTC with millisecond precision.

## Quick Start

Prerequisites: Rust 1.88+ (current stable) and, for AppService mode, server-side
access to a Matrix homeserver.

```bash
# Generate an AppService registration file
cumments generate-registration --server-name your_server.tld
```

Place the generated `registration.yaml` on the homeserver, put the printed
`as_token` / `hs_token` into `config.toml`, then run:

```bash
mkdir -p data
RUST_LOG=info cargo run -p cumments
```

### Docker

```bash
docker build -t cumments -f misc/docker/Dockerfile .
docker run -p 7931:7931 -v $(pwd)/data:/app/data cumments
```

The image starts in `logging` mode by default. Override it for production with
environment variables, e.g.:

```bash
docker run -p 7931:7931 \
  -e CUMMENTS__MATRIX__MODE=appservice \
  -e CUMMENTS__MATRIX__SERVER_NAME=your_server.tld \
  -e CUMMENTS__MATRIX__AS_TOKEN=... \
  -e CUMMENTS__MATRIX__HS_TOKEN=... \
  cumments
```

The container healthcheck uses `GET /health`.

## CLI

```text
cumments generate-registration --server-name <domain> [--url <url>] [--quiet]
cumments backfill
cumments backup --output <file>
```

## API

### Challenge

`GET /api/challenge`

```json
{
  "prefix": "timestamp_hex.random_hex.signature",
  "difficulty": 4
}
```

Challenges expire after 5 minutes.

### Health

`GET /health`

```json
{ "status": "ok" }
```

### Comments

All write operations require `author_public_key` (base64url Ed25519, 32 bytes)
and `author_signature` over a canonical message. The PoW `challenge_prefix` is
the part of `challenge_response` before `|`.

**List comments**

`QUERY /api/sites/{site_id}/posts/{post_slug}/comments` (RFC 10008)

Body:

```json
{ "page": 1, "per_page": 20 }
```

Response:

```json
{
  "data": [
    {
      "event_id": "$event:server",
      "site_id": "my-blog",
      "post_slug": "hello-world",
      "author_nickname": "Alice",
      "author_public_key": "...",
      "content": "...",
      "timestamp": "2026-08-08T00:00:00Z"
    }
  ],
  "meta": {
    "total": 1,
    "page": 1,
    "per_page": 20,
    "total_pages": 1
  }
}
```

**Post a comment**

`POST /api/sites/{site_id}/posts/{post_slug}/comments`

Body:

```json
{
  "content": "...",
  "nickname": "Alice",
  "email": null,
  "author_public_key": "...",
  "author_signature": "...",
  "challenge_response": "challenge|nonce"
}
```

Signature message:

```text
POST\n{site_id}\n{post_slug}\n{content}\n{nickname}\n{challenge_prefix}
```

**Edit a comment**

`PATCH /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

Signature message:

```text
PATCH\n{site_id}\n{post_slug}\n{comment_id}\n{content}\n{challenge_prefix}
```

**Delete a comment**

`DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

Signature message:

```text
DELETE\n{site_id}\n{post_slug}\n{comment_id}\n{challenge_prefix}
```

### Real-time updates (SSE)

`GET /api/sites/{site_id}/posts/{post_slug}/sse`

Server-sent events use the shape `{ "type": "...", "payload": { ... } }`:

```text
type: new_comment
type: comment_updated
type: comment_deleted
```

The `new_comment` and `comment_updated` payloads contain the full `Comment`
object; `comment_deleted` contains the deleted `event_id`.

## Frontend Integration

`misc/frontend/index.html` is a standalone demo styled as a real comment
section: posting, editing/deleting your own comments, pagination, SSE, and a
“My comments” management view. It defaults to `http://localhost:7931`.

### Identity

Generate an Ed25519 keypair with WebCrypto and keep the private key in the
browser. The **public key is the identity**: send it as `author_public_key`,
and sign the canonical request message with the private key. Edit/delete are
authorized by comparing the presented public key to the one stored with the
comment and verifying the signature.

### Proof of Work

1. Call `GET /api/challenge`.
2. Find a `nonce` such that `SHA256(prefix + nonce)` starts with `difficulty`
   leading zero hex digits.
3. Submit `challenge_response = prefix + "|" + nonce`.

### Validation

`site_id` and `post_slug` accept `[a-zA-Z0-9_-]`, 1–64 characters. Invalid
values return `400 VALIDATION_ERROR`.

## Development

```bash
cargo fmt --all -- --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

CI runs the same commands on GitHub Actions.

## Known Limitations

- `reply_to` and `email` are accepted by the API but are not yet written to
  Matrix events or the read model.
- Rate limiting, reply trees, and multi-instance/Postgres support are not
  implemented yet.
- `backfill` has unit tests, but end-to-end validation against a real Synapse
  deployment is still pending.

## License

MIT

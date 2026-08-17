# Cumments

[English](README.md) | [中文](README.zh-CN.md)

**Documentation:** <https://curious-r.github.io/cumments/>

Cumments is a decentralized comment system backend built on the **Matrix
protocol**. Matrix is the **source of truth**: every comment, edit and
deletion is an immutable Matrix event. SQLite is a disposable local read model
that can be rebuilt from Matrix history with `cumments backfill`.

## Highlights

- **Matrix as the event log** — comments are `m.room.message` events, edits
  are `m.replace`, and deletions are `m.redaction`.
- **Two kinds of authors** — visitor comments posted through the API carry the
  author's Ed25519 public key, signature and signed PoW challenge; Matrix
  comments are governed by Matrix identities and room power levels.
- **Disposable read model** — `cumments backfill` rebuilds sites, rooms and
  comments from Matrix history.
- **AppService-first** — production mode registers as a Matrix Application
  Service, uses virtual users, and receives events via HTTP push.
- **PoW anti-spam** — visitor comments require a signed proof-of-work challenge;
  no login or account system.
- **Reply trees and real-time SSE** — replies use Matrix rich replies, and
  updates stream via `message_created` / `message_updated` / `message_deleted`.

## Quick start (Docker)

The bundled compose file starts a minimal local stack — tuwunel plus Cumments
— with everything configured through environment variables:

```bash
mkdir -p ~/cumments-demo && cd ~/cumments-demo
cp /path/to/cumments/misc/docker/compose.yaml docker-compose.yml
docker run --rm --entrypoint cumments \
  ghcr.io/curious-r/cumments:latest \
  appservice generate-registration \
  --server-name localhost:8008 \
  --url http://cumments:7931 > registration.yaml
# Replace the <as_token>/<hs_token> placeholders in docker-compose.yml, then:
docker compose up -d
```

The full walkthrough — registration, the first site admin, verification and
troubleshooting — is in the [installation guide](docs/quick-start.md).

## Documentation

The full documentation is rendered at <https://curious-r.github.io/cumments/>.

**Getting started**

- [Installation](docs/quick-start.md) — quick start with the official image
  and a homeserver.
- [Configuration](docs/configuration.md) — config discovery, environment
  variables, full AppService example.

**Concepts**

- [Architecture](docs/architecture.md) — system design, operation modes,
  recovery, crates.
- [Data model](docs/data-model.md) — the Matrix-to-comment mapping and
  storage layout.
- [Site authentication](docs/site-trust.md) — origin and HMAC
  write-path trust models.
- [Site verification](docs/site-verification.md) — bind an SSG site,
  well-known/DNS proofs, strict HMAC mode.
- [Site governance](docs/site-governance.md) — site admins, managers and
  per-room moderators in Matrix power levels.

**Reference**

- [API](docs/api/index.md) — challenge, comments, signatures, SSE, design
  trade-offs, split by resource area.
- [CLI](docs/cli.md) — local administration: sites, rooms, roles, backups.
- [Problem types](docs/problems/index.md) — the RFC 9457 error registry.

**Development**

- [Development](docs/development.md) — toolchain, checks, building the image
  from main.
- [Demo](docs/demo.md) — backend-only positioning, demo page, identity,
  proof of work.

## License

MIT

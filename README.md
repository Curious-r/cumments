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
- **Two kinds of authors** — guest comments posted through the API carry the
  author's Ed25519 public key, signature and signed PoW challenge; Matrix
  comments are governed by Matrix identities and room power levels.
- **Disposable read model** — `cumments backfill` rebuilds sites, rooms and
  comments from Matrix history.
- **AppService-first** — production mode registers as a Matrix Application
  Service, uses virtual users, and receives events via HTTP push.
- **PoW anti-spam** — guest comments require a signed proof-of-work challenge;
  no login or account system.
- **Reply trees and real-time SSE** — replies use Matrix rich replies, and
  updates stream via `comment_created` / `comment_updated` / `comment_deleted`.

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

The full walkthrough — registration, the site owner, verification and
troubleshooting — is in the [installation guide](docs/installation.md).

## Documentation

| Guide | Description |
|---|---|
| [Online documentation](https://curious-r.github.io/cumments/) | Rendered site: API, configuration, problem types |
| [Installation](docs/installation.md) | Quick start with the official image and a homeserver |
| [Configuration](docs/configuration.md) | Config discovery, environment variables, full example |
| [Architecture](docs/architecture.md) | System design, operation modes, recovery, crates |
| [API](docs/api.md) | Challenge, comments, signatures, SSE |
| [Site verification](docs/site-verification.md) | Bind an SSG site, well-known/DNS proofs, strict HMAC mode |
| [CLI](docs/cli.md) | Local administration: sites, rooms, backup, completions |
| [Demo](docs/demo.md) | Backend-only positioning, demo page, identity, proof of work |
| [Development](docs/development.md) | Toolchain, CLI, building the image from main |

## License

MIT

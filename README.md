# Cumments

[English](README.md) | [中文](README.zh-CN.md)

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

```bash
docker run --rm --entrypoint cumments \
  ghcr.io/curious-r/cumments:latest \
  generate-registration \
  --server-name your_server.tld \
  --url http://cumments:7931
```

Save the printed `registration.yaml` and tokens, register the appservice with
your Matrix homeserver, mount the config and data directories, and start it
with the compose block in [`misc/docker/compose.yaml`](misc/docker/compose.yaml).
The full walkthrough — directory layout, tuwunel registration, configuration,
verification and troubleshooting — is in the [installation guide](docs/installation.md).

## Documentation

| Guide | Description |
|---|---|
| [Installation](docs/installation.md) | Quick start with the official image and a homeserver |
| [Configuration](docs/configuration.md) | Config discovery, environment variables, full example |
| [Architecture](docs/architecture.md) | System design, operation modes, recovery, crates |
| [API](docs/api.md) | Challenge, comments, signatures, SSE |
| [Frontend](docs/frontend.md) | Demo page, identity, proof of work |
| [Development](docs/development.md) | Toolchain, CLI, building the image from main |

## License

MIT

# Cumments

Cumments is a Matrix-native comment system. Comments are first-class Matrix
events: comments, edits and deletes live in Matrix rooms, are stored by a
projector into a local read model, and are exposed to websites through an
HTTP API with async write submissions.

Matrix is the **source of truth**. SQLite is only a disposable read model:
`cumments backfill` rebuilds it from Matrix history.

## Getting started

- [Quick start](quick-start.md) — run the bundled tuwunel + Cumments stack
  and register the first site admin.
- [Configuration](configuration.md) — file discovery, environment variables
  and the full AppService example.

## Concepts

- [Architecture](architecture.md) — the design philosophy (Matrix as truth,
  one write seam, disposable projection), how the API, reconciler, projector
  and AppService fit together, plus recovery.
- [Data model](data-model.md) — how Matrix events map to the typed comment
  model and its storage layout.
- [Site trust](site-trust.md) — origin and HMAC write-path trust
  models, with a [verification walkthrough](site-verification.md) for SSG
  sites.
- [Site governance](site-governance.md) — site admins, managers and per-room
  moderators encoded in Matrix power levels, with token-DM verification.

## Reference

- [API](api/index.md) — shared primitives, then per-area references for
  [comments](api/comments.md), [sites](api/sites.md),
  [governance](api/governance.md), [operator](api/operator.md) and
  [media](api/media.md).
- [Problem types](problems/index.md) — the RFC 9457 error registry.
- [CLI](cli.md) — local administration of sites, rooms, roles and backups.

## Development

- [Development](development.md) — toolchain, checks and image builds.

Cumments is backend/API infrastructure.

The visitor-facing frontend is maintained separately:
https://github.com/Curious-r/cumments-web

The OpenAPI contract lives at [`/openapi.yaml`](/openapi.yaml) and is validated
by CI (Redocly lint).

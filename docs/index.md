# Cumments

Cumments is a Matrix-native comment system. Comments are first-class Matrix
events: posts, edits and deletes live in Matrix rooms, are stored by a
projector into a local read model, and are exposed to websites through an
HTTP API with async write intents.

The documentation is organized as follows:

- [API reference](api.md) — endpoints, signatures, idempotent writes and
  real-time updates.
- [Problem types](problems/index.md) — the RFC 9457 error registry.
- [Configuration](configuration.md) — server, database, security and Matrix
  appservice settings.
- [Installation](installation.md) and [Development](development.md) —
  deployment and local development.
- [Architecture](architecture.md) — how the reconciler, projector and
  appservice fit together.
- [Site authentication](site-authentication.md) and
  [Site verification](site-verification.md) — write-path trust models.
- [Demo](demo.md) — the standalone browser demo page.

The OpenAPI contract lives at [`openapi.yaml`](openapi.yaml) and is kept in
sync with the API implementation by CI.

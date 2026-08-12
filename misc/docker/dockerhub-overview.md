# Cumments

Cumments is a Matrix-native comment system backend. Matrix is the **source of
truth**: every comment, edit and deletion is an immutable Matrix event.
SQLite is only a disposable local read model that can be rebuilt from Matrix
history with `cumments backfill`.

## Features

- **Matrix as the event log** — comments are `m.room.message`, edits are
  `m.replace`, deletions are `m.redaction`.
- **Guest comments without accounts** — Ed25519 identity, signed Proof-of-Work
  anti-spam, optional reply trees.
- **Real-time SSE** — `comment_created` / `comment_updated` /
  `comment_deleted` events.
- **AppService-first** — registers as a Matrix Application Service, uses
  virtual users, receives events over HTTP push.
- **Flexible site trust** — operator-declared `[sites]` or self-service
  verification via `/.well-known` / DNS TXT; origin mode or HMAC secret mode.
- **Admin API + local CLI** — sites, secrets, blocked rooms, backups and
  shell completions.

## Supported platforms

- linux/amd64
- linux/arm64

## Tags

- `latest` — latest build from `main`
- `0.20.1` — versioned multi-arch release (plus `0.20.1-amd64` /
  `0.20.1-arm64` for pinning a single architecture)
- `sha-<commit>` — manual publish builds

## Quick start

The image needs a Matrix homeserver (the repo ships a minimal
tuwunel + Cumments compose stack). Generate the AppService registration
first:

```bash
docker run --rm --entrypoint cumments \
  curiousss/cumments:latest \
  appservice generate-registration \
  --server-name your-server.example.com \
  --url http://cumments:7931 > registration.yaml
```

Then run the server, mounting the data volume, the registration file and your
configuration:

```yaml
services:
  cumments:
    image: curiousss/cumments:latest
    restart: unless-stopped
    ports:
      - "7931:7931"
    volumes:
      - cumments-data:/srv/cumments
      - ./registration.yaml:/etc/cumments/registration.yaml:ro
      - ./cumments.toml:/etc/cumments/cumments.toml:ro
    environment:
      CUMMENTS__SERVER__HOST: 0.0.0.0
      CUMMENTS__SERVER__PORT: 7931
      CUMMENTS__DATABASE__URL: sqlite:///srv/cumments/cumments.db
      CUMMENTS__SECURITY__POW_SECRET: "<openssl rand -hex 32>"
      CUMMENTS__MATRIX__MODE: appservice
      CUMMENTS__MATRIX__HOMESERVER__ADDRESS: http://tuwunel:6167
      CUMMENTS__MATRIX__HOMESERVER__DOMAIN: your-server.example.com
      CUMMENTS__MATRIX__APPSERVICE__ID: cumments
      CUMMENTS__MATRIX__APPSERVICE__URL: http://cumments:7931
      CUMMENTS__MATRIX__APPSERVICE__LISTEN_HOST: 0.0.0.0
      CUMMENTS__MATRIX__APPSERVICE__LISTEN_PORT: 7931
      CUMMENTS__MATRIX__APPSERVICE__SENDER_LOCALPART: _cumments_bot
      CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN: "<as_token>"
      CUMMENTS__MATRIX__APPSERVICE__HS_TOKEN: "<hs_token>"
      CUMMENTS__MATRIX__APPSERVICE__REGISTRATION_FILE: /etc/cumments/registration.yaml
      CUMMENTS__MATRIX__MODERATION__ADMIN_ID: "@admin:your-server.example.com"

volumes:
  cumments-data:
```

The full tuwunel + Cumments stack lives in the repository at
`misc/docker/compose.yaml`.

## Configuration

- Configuration file: `/etc/cumments/cumments.toml` (mounted read-only).
- Environment variables use the `CUMMENTS__SECTION__KEY` pattern, e.g.
  `CUMMENTS__SECURITY__POW_SECRET`.
- Persistent data: `/srv/cumments` (SQLite database, backups, registration).
- HTTP/AppService listener: `7931`.
- Health check: `GET /health` on the server port.

## Documentation

Full docs, API reference and problem-type registry:
<https://curious-r.github.io/cumments/>

## License

MIT

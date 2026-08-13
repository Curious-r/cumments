# Cumments

Cumments is a Matrix-native comment system backend. Matrix is the **source of
truth**: every comment, edit and deletion is an immutable Matrix event.
SQLite is only a disposable local read model that can be rebuilt from Matrix
history with `cumments backfill`.

## Features

- **Matrix as the event log** — comments are `m.room.message`, edits are
  `m.replace`, deletions are `m.redaction`.
- **Two kinds of authors** — guest comments without accounts carry an
  Ed25519 identity, signature and signed Proof-of-Work challenge; Matrix
  users comment natively and are governed by room power levels.
- **Rich content** — guest uploads (image / video / audio / file / voice)
  served through a signed public media proxy, MSC3488 locations, reactions
  and MSC3381 polls.
- **Reply trees and real-time SSE** — `message_created` /
  `message_updated` / `message_deleted`, plus typing, read receipts and
  presence events.
- **AppService-first** — registers as a Matrix Application Service, uses
  virtual users, receives events over HTTP push.
- **Flexible site trust** — operator-declared `[sites]` or self-service
  verification via `/.well-known` / DNS TXT; origin mode or HMAC secret mode.
- **Site governance** — owner (100), co-manager (75) and per-room moderator
  (50) roles encoded in Matrix power levels; owners manage everything from a
  Matrix client, the API writes to the same state.
- **Admin API + local CLI** — sites, secrets, roles, quarantined rooms,
  backups and shell completions.
- **Rebuildable read model** — `cumments backfill` reconstructs SQLite from
  Matrix history.

## Supported platforms

- linux/amd64
- linux/arm64

## Tags

- `latest` — latest build from `main`
- `0.21.0` — versioned multi-arch release (plus `0.21.0-amd64` /
  `0.21.0-arm64` for pinning a single architecture)
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

volumes:
  cumments-data:
```

The full tuwunel + Cumments stack lives in the repository at
`misc/docker/compose.yaml`.

After the first start, register a site and bind the site owner's Matrix
account once:

```bash
curl -sS -X POST http://localhost:7931/api/v1/sites > site.json
curl -sS -X POST "http://localhost:7931/api/v1/sites/$(jq -r .site_id site.json)/owners" \
  -H "Content-Type: application/json" \
  -H "X-Cumments-Claim-Token: $(jq -r .claim_token site.json)" \
  -d '{"user_id":"@you:your-server.example.com"}'
```

Everything else — appointing co-managers, moderating rooms — happens from a
Matrix client. See the [site governance
guide](https://curious-r.github.io/cumments/site-governance/) and the
[installation guide](https://curious-r.github.io/cumments/quick-start/).

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

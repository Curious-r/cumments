# Quick start

This guide follows the AppService deployment flow: a Matrix homeserver (the
examples use [tuwunel](https://github.com/matrix-construct/tuwunel)) plus the
official Cumments image from GHCR.

## Prerequisites

- Docker with Compose (v2).
- A Matrix homeserver reachable from the Cumments container, and a Matrix
  domain (`your_server.tld` below).
- A place where the homeserver can reach Cumments (in Compose, the service
  name is enough, e.g. `http://cumments:7931`).

## 1. Prepare the directory layout

```text
deploy/
├── docker-compose.yml        # or your existing compose project
├── cumments.env              # env_file for the Cumments service
├── cumments/
│   ├── config/               # mounted read-only at /etc/cumments
│   │   ├── cumments.toml
│   │   └── registration.yaml
│   └── data/                 # mounted read-write at /srv/cumments (SQLite)
```

## 2. Generate the AppService registration

```bash
docker run --rm --entrypoint cumments \
  ghcr.io/curious-r/cumments:latest \
  generate-registration \
  --server-name your_server.tld \
  --url http://cumments:7931
```

- `--server-name` is the Matrix ID domain (the part after `:` in user IDs).
- `--url` is the callback URL the homeserver uses to push events. Inside
  Compose this is the service name, e.g. `http://cumments:7931`; behind a
  reverse proxy use the public URL.
- The command prints `registration.yaml` on stdout and the matching
  `as_token` / `hs_token` on stderr. Save the YAML as
  `cumments/config/registration.yaml` and note both tokens.

## 3. Register the appservice with the homeserver

For tuwunel, put `registration.yaml` where tuwunel loads appservice
registrations and restart it, then verify from the admin room:

```text
!admin appservices list
```

Other homeservers (Synapse, Conduit, ...) accept the same YAML through their
own appservice registration mechanism.

## 4. Configure Cumments

`cumments/config/cumments.toml`:

```toml
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"

[database]
url = "sqlite:///srv/cumments/cumments.db"

[security]
pow_secret = "replace-with-a-long-random-secret"
pow_difficulty = 4

[matrix]
mode = "appservice"

[matrix.homeserver]
address = "http://tuwunel:6167"   # reachable from the Cumments container
domain = "your_server.tld"

[matrix.appservice]
id = "cumments"
url = "http://cumments:7931"      # callback URL, reachable from the homeserver
listen_host = "0.0.0.0"
listen_port = 3001
sender_localpart = "_cumments_bot"
# Prefer environment variables over committing tokens to files:
# CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN=...
# CUMMENTS__MATRIX__APPSERVICE__HS_TOKEN=...
# as_token = "<as_token from step 2>"
# hs_token = "<hs_token from step 2>"
registration_file = "/etc/cumments/registration.yaml"
# room_version = "12"   # optional; see docs/configuration.md

[matrix.moderation]
owner_id = "@admin:your_server.tld"
```

`cumments.env` (env_file used by Compose):

```dotenv
CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN=<as_token from step 2>
CUMMENTS__MATRIX__APPSERVICE__HS_TOKEN=<hs_token from step 2>
RUST_LOG=info
```

## 5. Start it

Add the service to the compose project that defines `tuwunel` (or adjust the
network name). The ready-made block lives in `misc/docker/compose.yaml`:

```yaml
services:
  cumments:
    image: ghcr.io/curious-r/cumments:latest
    env_file: cumments.env
    restart: unless-stopped
    volumes:
      - ./cumments/config/:/etc/cumments/:ro
      - ./cumments/data/:/srv/cumments/:rw
    depends_on:
      - tuwunel
    networks:
      - tuwunel
```

```bash
docker compose up -d
docker compose logs -f cumments
```

You should see `Configuration loaded successfully.`, `Database initialized.`,
and `Server listening on 0.0.0.0:7931`. The entrypoint fixes the ownership of
`/srv/cumments` and drops to the unprivileged `cumments` user; set
`PUID`/`PGID` in the environment to make data files owned by your host user.

## 6. Verify

1. Open the [demo frontend](demo.md) (`misc/demo/index.html`) against the API
   and post a comment.
2. In Matrix, check that a Space (`Comments: <site>`), a comment room
   (`Comments: <site>/<post>`), and the virtual user were created.
3. The comment should appear in the frontend in real time via SSE.

If a comment never appears, the read model can be rebuilt from Matrix history:

```bash
docker compose exec cumments cumments backfill
```

## Optional: room version 12

Room version 12 hardens rooms (hash-based room IDs, immutable creator power).
On tuwunel, set `default_room_version = "12"` in the homeserver config, or
request v12 per room with `room_version = "12"` in
`[matrix.appservice]` (see docs/configuration.md).

## Troubleshooting

- **`unknown field` on startup**: the running binary is older than the config
  file. Rebuild/re-pull the image (`docker compose pull`) or check that a
  locally built test image actually contains the latest main (the `git clone`
  RUN layer is cached by Docker; build with
  `docker compose build --no-cache` or a changing build arg).
- **`invalid hs_token` warnings**: the `hs_token` in the config/env does not
  match the registration file registered on the homeserver.
- **Comments exist in Matrix but not in the API**: the push queue was blocked
  or a transaction was never acked; restart the service and, if needed, run
  `cumments backfill`.
